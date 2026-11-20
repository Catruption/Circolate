//! Structured report of how a song's sources resolved.
//!
//! This is deliberately read-only: it loads a project, prints one JSON
//! document, and exits.  No audio is rendered, so "why is this part
//! silent?" can be answered without a terminal, a sound card, or a
//! second build.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::deps::plugins::PluginTable;
use crate::player;

pub fn run(args: &[String]) -> Result<()> {
    let mut json_only = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--json" | "-j" => json_only = true,
            "-h" | "--help" => {
                println!("usage: circolate inspect <FILE> [--json]");
                println!();
                println!("  --json, -j  emit the resolution report as JSON");
                return Ok(());
            }
            _ => positional.push(arg.clone()),
        }
    }
    let song = positional
        .first()
        .ok_or_else(|| anyhow!("inspect needs a song file"))?;

    let path = crate::paths::resolve_track(song)?;
    let (project, _cache) = player::load_project(&path, 44_100, true, true)?;
    let table = PluginTable::get();

    let sources: Vec<Value> = project
        .resolution_report
        .iter()
        .map(|r| {
            json!({
                "part": r.part,
                "kind": r.kind,
                "name": r.name,
                "status": r.status,
                "reason": r.reason,
                "fallback": r.fallback,
            })
        })
        .collect();
    let missing = project
        .resolution_report
        .iter()
        .filter(|r| r.status == "missing")
        .count();

    let report = json!({
        "title": project.title,
        "sample_rate": project.sample_rate,
        "parts": project.parts.len(),
        "voices": project.voice_count,
        "sources": sources,
        "summary": {
            "sources": project.resolution_report.len(),
            "missing": missing,
            "plugins_found": table.total(),
        },
    });

    if json_only {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for source in &project.resolution_report {
            println!(
                "{:<6} {:<40} {:<9} {}",
                source.kind,
                source.part,
                source.status,
                source.reason.as_deref().unwrap_or("")
            );
        }
        println!(
            "{}: {} sources, {} missing",
            project.title,
            project.resolution_report.len(),
            missing
        );
        eprintln!("use --json for the full report");
    }
    Ok(())
}
