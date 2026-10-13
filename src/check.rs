//! `trackscript-player check` — recursively probe .flp files for missing deps.
//!
//! Never converts, never renders, never writes.  Exit code is 1 if any
//! file has missing dependencies (or a fallback whose target is absent),
//! so it composes with `find`, CI, and `run.sh check`.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::convert::{self, ConvertOptions, DepKind, ProbeReport};

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
                    bail!("check takes at most one path (got extra: {other})");
                }
                path = Some(PathBuf::from(other));
            }
        }
    }

    let root = path.unwrap_or_else(|| PathBuf::from("."));
    if !root.exists() {
        bail!("no such path: {}", root.display());
    }

    let opts = ConvertOptions::default();

    // A single file goes straight in; only recurse if we were handed
    // a directory.
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

    // … rest unchanged

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
                total_files     += 1;
                total_missing   += rep.missing.len();
                total_fallbacks += rep.fallbacks.len();
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
    println!("usage: trackscript-player check [--json] [PATH]");
    println!();
    println!("  Recursively probe .flp files for missing dependencies.");
    println!("  PATH defaults to the current directory.");
    println!();
    println!("  --json    emit one JSON object per file, one per line");
    println!();
    println!("  Exit status is 1 if any file has missing deps.");
}

fn collect_flps(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let rd = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?;
    for entry in rd {
        let entry = entry?;
        let ty = entry.file_type()?;
        let p = entry.path();
        if ty.is_dir() {
            collect_flps(&p, out)?;
        } else if ty.is_file()
            && p.extension()
                .and_then(|s| s.to_str())
                .map_or(false, |e| e.eq_ignore_ascii_case("flp"))
        {
            out.push(p);
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
        println!(
            "        [{:>3}] fallback {} -> {} ({})",
            f.channel_iid,
            f.replaces,
            f.used,
            if f.resolved { "resolved" } else { "MISSING" },
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
        })).collect::<Vec<_>>(),
        "warnings": rep.warnings,
    });
    println!("{}", serde_json::to_string(&obj)?);
    Ok(())
}