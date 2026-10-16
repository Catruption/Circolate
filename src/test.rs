//! `trackscript-player test` — single-file diagnostic.
//!
//! Unlike `check`, which just says clean / dirty per file, `test` prints
//! the parsed structure of one source and then runs the full probe
//! pipeline so you can see why each channel is (or isn't) convertible.
//!
//! Usage:
//!     trackscript-player test <file> [--json]
//!
//! Exit code is non-zero when the probe reports missing dependencies or
//! unresolved fallbacks, matching `check`.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::convert::{self, ConvertOptions, DepKind, ProbeReport};
use crate::formats::flp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Flp,
    Trackscript,
    Unknown,
}

fn detect_kind(p: &Path) -> Kind {
    match p
    .extension()
    .and_then(|s| s.to_str())
    .map(|s| s.to_ascii_lowercase())
    .as_deref()
    {
        Some("flp") => Kind::Flp,
        Some("trackscript") | Some("tsz") => Kind::Trackscript,
        _ => {
            // Fall back to magic sniffing so `test foo` on a renamed FLP
            // still works.
            if let Ok(bytes) = std::fs::read(p) {
                if bytes.len() >= 4 && &bytes[..4] == b"FLhd" {
                    return Kind::Flp;
                }
            }
            Kind::Unknown
        }
    }
}

pub fn run(argv: &[String]) -> Result<()> {
    let mut json = false;
    let mut path: Option<PathBuf> = None;

    for a in argv {
        match a.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => {
                if path.is_some() {
                    bail!("test takes at most one file (got extra: {other})");
                }
                path = Some(PathBuf::from(other));
            }
        }
    }

    let path = match path {
        Some(p) => p,
        None => bail!("test requires a file argument (try --help)"),
    };
    if !path.exists() {
        bail!("no such file: {}", path.display());
    }

    match detect_kind(&path) {
        Kind::Flp => test_flp(&path, json),
        Kind::Trackscript => test_trackscript(&path),
        Kind::Unknown => bail!(
            "unrecognised file type: {} (expected .flp, .trackscript or .tsz)",
                               path.display()
        ),
    }
}

fn print_help() {
    println!("usage: trackscript-player test <file> [--json]");
    println!();
    println!("  Parse a source file and report why each channel does or");
    println!("  does not convert.  Supported inputs: .flp, .trackscript, .tsz.");
    println!();
    println!("  --json    emit the report as a single JSON object");
    println!();
    println!("  Exit status is 1 if the probe finds missing dependencies.");
}

// ── FLP ────────────────────────────────────────────────────────────────

fn test_flp(path: &Path, json: bool) -> Result<()> {
    let prj = flp::parse_file(path)
    .with_context(|| format!("parsing {}", path.display()))?;

    if !json {
        print_flp_header(path, &prj);
        print_flp_channels(&prj);
        print_flp_patterns(&prj);
        print_flp_playlist(&prj);
    }

    let opts = ConvertOptions::default();
    let rep = match convert::probe_file(path, &opts) {
        Ok(r) => r,
        Err(e) => {
            if json {
                let obj = serde_json::json!({
                    "file": path.display().to_string(),
                                            "error": format!("{:#}", e),
                });
                println!("{}", serde_json::to_string(&obj)?);
            } else {
                println!();
                println!("probe failed: {:#}", e);
            }
            std::process::exit(1);
        }
    };

    if json {
        print_probe_json(path, &rep)?;
    } else {
        println!();
        print_probe(&rep);
    }

    if rep.problem_count() > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn print_flp_header(path: &Path, prj: &flp::Project) {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
    println!("== {} ==", name);
    println!(
        "version {}   ppq {}   tempo {:.3}",
        if prj.version.is_empty() { "?" } else { &prj.version },
            prj.ppq,
             prj.tempo,
    );
    if !prj.title.is_empty() {
        println!("title   {}", prj.title);
    }
    if !prj.author.is_empty() {
        println!("author  {}", prj.author);
    }
    println!(
        "{} channel(s), {} pattern(s), {} playlist item(s), {} insert(s)",
             prj.channels.len(),
             prj.patterns.len(),
             prj.playlist.len(),
             prj.inserts.len(),
    );
    if !prj.parse_warnings.is_empty() {
        println!("parse warnings:");
        for w in &prj.parse_warnings {
            println!("  ! {}", w);
        }
    }
    println!();
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn short_basename(p: &str) -> String {
    if p.is_empty() {
        return String::new();
    }
    let norm = p.replace('\\', "/");
    let base = norm.rsplit('/').next().unwrap_or(&norm);
    truncate(base, 32)
}

fn print_flp_channels(prj: &flp::Project) {
    // Count notes per channel across all patterns.
    let mut notes_per: std::collections::HashMap<u16, usize> =
    std::collections::HashMap::new();
    for p in &prj.patterns {
        for n in &p.notes {
            *notes_per.entry(n.channel).or_insert(0) += 1;
        }
    }

    println!("channels:");
    for c in &prj.channels {
        let (state_sz, internal, vst) = match &c.plugin {
            Some(p) => (
                p.state_size,
                p.internal_name.as_str(),
                        p.vst_path.as_str(),
            ),
            None => (0, "", ""),
        };
        let notes = notes_per.get(&c.iid).copied().unwrap_or(0);
        let enabled = match c.enabled {
            Some(false) => " [disabled]",
            _ => "",
        };

        let line = format!(
            "  [{:>3}] {:<10} notes={:>5} state={:>8} {}",
            c.iid,
            c.channel_type,
            notes,
            state_sz,
            truncate(if c.name.is_empty() { "<unnamed>" } else { &c.name }, 36),
        );
        println!("{}{}", line, enabled);

        let mut extras: Vec<String> = Vec::new();
        if !internal.is_empty() {
            extras.push(format!("internal={}", truncate(internal, 24)));
        }
        if !vst.is_empty() {
            extras.push(format!("vst={}", short_basename(vst)));
        }
        if !c.sample_path.is_empty() {
            extras.push(format!("sample={}", short_basename(&c.sample_path)));
        }
        if !c.envelopes.is_empty() {
            extras.push(format!("envs={}", c.envelopes.len()));
        }
        if !extras.is_empty() {
            println!("        {}", extras.join("  "));
        }
    }
    println!();
}

fn print_flp_patterns(prj: &flp::Project) {
    if prj.patterns.is_empty() {
        return;
    }
    println!("patterns:");
    for p in &prj.patterns {
        if p.notes.is_empty() {
            continue;
        }
        let mut chans: Vec<u16> = p.notes.iter().map(|n| n.channel).collect();
        chans.sort_unstable();
        chans.dedup();
        let name = if p.name.is_empty() { "<unnamed>" } else { &p.name };
        println!(
            "  [{:>3}] {:>5} note(s)  chans={:?}  {}",
                 p.iid,
                 p.notes.len(),
                 chans,
                 truncate(name, 32),
        );
    }
    println!();
}

fn print_flp_playlist(prj: &flp::Project) {
    if prj.playlist.is_empty() {
        return;
    }
    let mut pats = 0usize;
    let mut chans = 0usize;
    for it in &prj.playlist {
        match it.kind.as_str() {
            "pattern" => pats += 1,
            "channel" => chans += 1,
            _ => {}
        }
    }
    println!(
        "playlist: {} item(s) ({} pattern clip(s), {} channel clip(s))",
             prj.playlist.len(),
             pats,
             chans,
    );
    let show = prj.playlist.len().min(8);
    for it in prj.playlist.iter().take(show) {
        println!(
            "    pos={:>7} len={:>6} kind={:<8} ref={}",
            it.position, it.length, it.kind, it.ref_id,
        );
    }
    if prj.playlist.len() > show {
        println!("    … {} more", prj.playlist.len() - show);
    }
    println!();
}

// ── trackscript ────────────────────────────────────────────────────────

fn test_trackscript(path: &Path) -> Result<()> {
    // Text-based inspection only for now; not attempting to load it into
    // the audio engine.
    let text = std::fs::read_to_string(path)
    .with_context(|| format!("reading {}", path.display()))?;

    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
    println!("== {} ==", name);

    let mut lines = text.lines();
    let mut header_count = 0usize;
    for line in lines.by_ref() {
        if line.starts_with("//") || line.trim().is_empty() {
            continue;
        }
        println!("  {}", line);
        header_count += 1;
        if header_count >= 12 {
            break;
        }
    }
    let remaining = lines.count();
    println!("  … {} more line(s)", remaining);
    println!();
    println!("note: full trackscript diagnostics are not implemented yet.");
    Ok(())
}

// ── probe report printing ──────────────────────────────────────────────

fn kind_label(k: DepKind) -> &'static str {
    match k {
        DepKind::Sample => "sample",
        DepKind::Plugin => "plugin",
        DepKind::Sf2 => "sf2",
        DepKind::Unknown => "?",
    }
}

fn print_probe(rep: &ProbeReport) {
    if rep.is_clean() && rep.fallbacks.is_empty() {
        println!("probe: clean");
        return;
    }

    println!("probe:");
    for m in &rep.missing {
        println!(
            "  [{:>3}] {:<6} {:<28} {}",
            m.channel_iid,
            kind_label(m.kind),
                 truncate(&m.channel_name, 28),
                 m.reason,
        );
    }
    for f in &rep.fallbacks {
        println!(
            "  [{:>3}] fallback {} -> {} ({})",
                 f.channel_iid,
                 f.replaces,
                 f.used,
                 if f.resolved { "resolved" } else { "MISSING" },
        );
    }
    for w in &rep.warnings {
        println!("  note: {}", w);
    }
}

fn print_probe_json(path: &Path, rep: &ProbeReport) -> Result<()> {
    let obj = serde_json::json!({
        "file": path.display().to_string(),
                                "missing": rep.missing.iter().map(|m| serde_json::json!({
                                    "channel": m.channel_iid,
                                    "name":    m.channel_name,
                                    "kind":    kind_label(m.kind),
                                                                                        "reason":  m.reason,
                                })).collect::<Vec<_>>(),
                                "fallbacks": rep.fallbacks.iter().map(|f| serde_json::json!({
                                    "channel":  f.channel_iid,
                                    "replaces": f.replaces,
                                    "used":     f.used,
                                    "resolved": f.resolved,
                                })).collect::<Vec<_>>(),
                                "warnings": rep.warnings,
    });
    println!("{}", serde_json::to_string(&obj)?);
    Ok(())
}
