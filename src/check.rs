//! `trackscript-player check` — recursively probe .flp files for missing deps.
//!
//! Never converts, never renders, never writes.
//! Exit code is 1 if any file has missing dependencies (or a fallback
//! whose target is absent), so it composes with `find`, CI, and
//! `run.sh check`.
//!
//! Additional modes:
//!   `--scaffold-fallbacks`      seed `share/fallbacks.json`
//!   `--write-requirements FILE` write `required_plugins.json` from
//!                               the scanned corpus
//!
//! The `census` subcommand (via `run_census`) is a thin alias for
//! `check --write-requirements` with a default output path.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::convert::{self, ConvertOptions, DepKind, ProbeReport};

pub fn run(argv: &[String]) -> Result<()> {
    let mut json = false;
    let mut path: Option<PathBuf> = None;
    let mut write_req: Option<PathBuf> = None;
    let mut scaffold = false;

    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--scaffold-fallbacks" => scaffold = true,
            "--write-requirements" => {
                let p = it.next()
                .ok_or_else(|| anyhow!("--write-requirements needs a path"))?;
                write_req = Some(PathBuf::from(p));
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => {
                if path.is_some() {
                    bail!("check takes at most one path (got extra: {other})");
                }
                path = Some(PathBuf::from(other));
            }
        }
    }

    if scaffold {
        let Some(repo) = crate::paths::repo_root() else {
            bail!("--scaffold-fallbacks requires a git checkout (no Cargo.toml found)");
        };
        let share = repo.join("share");
        let required_path = share.join(crate::deps::census::CENSUS_FILENAME);
        let fallbacks_path = share.join(crate::deps::fallbacks::FALLBACKS_FILENAME);
        return scaffold_fallbacks(&required_path, &fallbacks_path);
    }

    if let Some(req_out) = write_req {
        let root = path.unwrap_or_else(|| PathBuf::from("."));
        return write_requirements(&root, &req_out);
    }

    let root = path.unwrap_or_else(|| PathBuf::from("."));
    if !root.exists() {
        bail!("no such path: {}", root.display());
    }

    let opts = ConvertOptions::default();

    let mut flps: Vec<PathBuf> = Vec::new();
    if root.is_file() {
        flps.push(root.clone());
    } else {
        collect_flps(&root, &mut flps)?;
        flps.sort();
    }

    if flps.is_empty() {
        eprintln!("no .flp files under {}", root.display());
        return Ok(());
    }

    let mut total_files      = 0usize;
    let mut files_clean      = 0usize;
    let mut files_with_prob  = 0usize;
    let mut total_missing    = 0usize;
    let mut total_fallbacks  = 0usize;
    let mut total_unresolved = 0usize;
    let mut failed           = 0usize;

    for flp in &flps {
        match convert::probe_file(flp, &opts) {
            Ok(rep) => {
                total_files      += 1;
                total_missing    += rep.missing.len();
                total_fallbacks  += rep.fallbacks.len();
                total_unresolved += rep.fallbacks.iter().filter(|f| !f.resolved).count();
                if rep.is_clean() { files_clean += 1; }
                if rep.problem_count() > 0 { files_with_prob += 1; }

                if json {
                    print_json(flp, &rep)?;
                } else {
                    print_human(flp, &root, &rep);
                }
            }
            Err(e) => {
                eprintln!("  FAIL  {}: {:#}", display_rel(flp, &root), e);
                failed += 1;
            }
        }
    }

    if !json {
        println!();
        println!(
            "{} file(s): {} clean, {} with problems, \
{} missing, {} fallback(s) ({} unresolved), {} failed",
                 total_files + failed,
                 files_clean,
                 files_with_prob,
                 total_missing,
                 total_fallbacks,
                 total_unresolved,
                 failed,
        );
    }

    if failed > 0 || total_missing > 0 || total_unresolved > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn print_help() {
    println!("usage: trackscript-player check [--json] [--scaffold-fallbacks] \\");
    println!("                           [--write-requirements FILE] [PATH]");
    println!();
    println!("  Recursively probe .flp files for missing dependencies.");
    println!("  PATH defaults to the current directory.");
    println!();
    println!("  --json                    emit one JSON object per file");
    println!("  --write-requirements FILE tally plugin names into required_plugins.json");
    println!("  --scaffold-fallbacks      seed fallbacks.json with empty rules");
    println!();
    println!("  Exit status is 1 if any file has missing deps.");
}

// ── census ─────────────────────────────────────────────────────────────

pub fn run_census(argv: &[String]) -> Result<()> {
    let mut path: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;

    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => {
                let p = it.next()
                .ok_or_else(|| anyhow!("--out needs a path"))?;
                out = Some(PathBuf::from(p));
            }
            "-h" | "--help" => {
                println!("usage: trackscript-player census [PATH] [--out FILE]");
                return Ok(());
            }
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => {
                if path.is_some() {
                    bail!("census takes at most one path (got extra: {other})");
                }
                path = Some(PathBuf::from(other));
            }
        }
    }

    let root = path.unwrap_or_else(|| PathBuf::from("."));
    let out = out.unwrap_or_else(|| {
        crate::paths::user_cache().join(crate::deps::census::CENSUS_FILENAME)
    });

    write_requirements(&root, &out)
}

fn write_requirements(root: &Path, out: &Path) -> Result<()> {
    if !root.exists() {
        bail!("no such path: {}", root.display());
    }

    let opts = ConvertOptions::default();
    let mut flps: Vec<PathBuf> = Vec::new();
    if root.is_file() {
        flps.push(root.to_path_buf());
    } else {
        collect_flps(root, &mut flps)?;
        flps.sort();
    }

    if flps.is_empty() {
        bail!("no .flp files under {}", root.display());
    }

    let mut census = crate::deps::census::RequiredPlugins::new(root);
    let mut failed = 0;

    for flp in &flps {
        match convert::probe_file(flp, &opts) {
            Ok(rep) => {
                for m in &rep.missing {
                    let is_real = m.channel_name != "?" && !m.channel_name.is_empty();
                    if matches!(m.kind, DepKind::Plugin) && is_real {
                        census.add(&m.channel_name, flp);
                    }
                }
                for f in &rep.fallbacks {
                    census.add(&f.replaces, flp);
                }
            }
            Err(e) => {
                eprintln!("  census skip {}: {:#}", flp.display(), e);
                failed += 1;
            }
        }
    }

    census.finalize();
    census.save(out)?;

    println!(
        "census: {} plugin(s) required across {} file(s), {} failed to probe",
             census.required.len(), flps.len(), failed
    );
    println!("wrote {}", out.display());
    println!();

    let top: Vec<_> = census.sorted_names().into_iter().take(30).collect();
    if !top.is_empty() {
        println!("Top requirements by occurrence:");
        for (name, count) in top {
            println!("  {:>4}  {}", count, name);
        }
    }

    Ok(())
}

// ── scaffold ───────────────────────────────────────────────────────────

fn scaffold_fallbacks(required_path: &Path, fallbacks_path: &Path) -> Result<()> {
    use crate::deps::fallbacks::FallbacksTable;

    let req = crate::deps::census::RequiredPlugins::load(required_path);
    if req.required.is_empty() {
        bail!(
            "{} is empty or missing; run `census` first",
            required_path.display()
        );
    }

    let mut fb = FallbacksTable::load(fallbacks_path)
    .unwrap_or_else(FallbacksTable::default);

    let before = fb.instruments.len();
    let names = req.names();
    fb.scaffold_missing(&names);
    fb.save(fallbacks_path)?;

    let added = fb.instruments.len().saturating_sub(before);
    let empty_count = fb.instruments.values().filter(|v| v.is_empty()).count();
    println!(
        "fallbacks: {} rule(s) total ({} new), {} still empty",
             fb.instruments.len(), added, empty_count
    );
    println!("wrote {}", fallbacks_path.display());

    if empty_count > 0 {
        println!();
        println!("Empty rules (need candidates), sorted by frequency:");
        let mut empties: Vec<(&String, u32)> = fb
        .instruments
        .iter()
        .filter(|(_, v)| v.is_empty())
        .map(|(k, _)| {
            let count = req.required.get(k).map(|r| r.occurrences).unwrap_or(0);
            (k, count)
        })
        .collect();
        empties.sort_by(|a, b| {
            b.1.cmp(&a.1).then_with(|| a.0.as_str().cmp(b.0.as_str()))
        });
        for (name, count) in &empties {
            println!("  {:>4}x  {}", count, name);
        }
    }

    Ok(())
}

// ── helpers ────────────────────────────────────────────────────────────

fn collect_flps(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let rd = std::fs::read_dir(dir)
    .with_context(|| format!("reading {}", dir.display()))?;
    for entry in rd {
        let entry = entry?;
        let ty = entry.file_type()?;
        let p = entry.path();

        if ty.is_dir() {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if matches!(
                name,
                "__MACOSX" | ".Trashes" | ".git" | "target"
                | "node_modules" | "__pycache__"
            ) {
                continue;
            }
            collect_flps(&p, out)?;
        } else if ty.is_file() {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with("._") {
                continue;
            }
            if p.extension()
                .and_then(|s| s.to_str())
                .map_or(false, |e| e.eq_ignore_ascii_case("flp"))
                {
                    out.push(p);
                }
        }
    }
    Ok(())
}

fn display_rel(p: &Path, root: &Path) -> String {
    let stripped = p.strip_prefix(root).unwrap_or(p);
    if stripped.as_os_str().is_empty() {
        p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("(unknown)")
        .to_string()
    } else {
        stripped.display().to_string()
    }
}

fn kind_label(k: DepKind) -> &'static str {
    match k {
        DepKind::Sample  => "sample",
        DepKind::Plugin  => "plugin",
        DepKind::Sf2     => "sf2",
        DepKind::Unknown => "?",
    }
}

fn print_human(flp: &Path, root: &Path, rep: &ProbeReport) {
    let rel = display_rel(flp, root);

    if rep.is_clean() {
        println!("  ok    {}", rel);
        return;
    }

    let tag = if rep.problem_count() > 0 { "MISS" } else { "warn" };
    println!("  {:<5} {}", tag, rel);

    for m in &rep.missing {
        println!(
            "        [{:>3}] {:<6} {:<24} {}",
            m.channel_iid,
            kind_label(m.kind),
                 m.channel_name,
                 m.reason,
        );
    }
    for f in &rep.fallbacks {
        let status = if f.via_default {
            "default — needs tuning"
        } else if f.resolved {
            "resolved"
        } else {
            "MISSING"
        };
        println!(
            "        [{:>3}] fallback {} -> {} ({})",
                 f.channel_iid,
                 f.replaces,
                 f.used,
                 status,
        );
    }
    for w in &rep.warnings {
        println!("        note: {}", w);
    }
}

fn print_json(flp: &Path, rep: &ProbeReport) -> Result<()> {
    let obj = serde_json::json!({
        "file": flp.display().to_string(),
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
                                    "via_default": f.via_default,
                                })).collect::<Vec<_>>(),
                                "warnings": rep.warnings,
    });
    println!("{}", serde_json::to_string(&obj)?);
    Ok(())
}
