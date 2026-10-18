// src/measure.rs
//! Loudness measurement for fallback plugins.
//!
//! Renders through the same code path as `vst-smoke` (see
//! `hosting::vst::smoke::render_stats`). Sharing the renderer is the
//! whole point: if smoke says a plugin plays, measure must agree.
//! Two separate loops inevitably diverge — the previous version used
//! `process_inplace()`, which is the *effect* entry point, so every
//! instrument measured `-inf` while smoke saw audio.
//!
//! Output written to `share/plugins_inventory.json` and consumed by
//! `deps::fallbacks` to compute fallback gains.

use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};

use crate::deps::fallbacks::{FallbacksTable, FALLBACKS_FILENAME};
use crate::deps::index::PluginIndex;
use crate::deps::inventory::{PluginInventory, PluginLevel, INVENTORY_FILENAME};
use crate::hosting::vst::smoke;

const MEASURE_DURATION_SECS: f32 = 4.0;

/// Measure a single plugin's default-patch output.
///
/// Returns `(rms_db, peak_db)`. `rms_db` is what most rules should use;
/// `peak_db` is useful for percussive instruments where the loudness
/// lives in the transients. Either can be `f32::NEG_INFINITY` if the
/// plugin produces silence (sampler without a preset, effect rather
/// than instrument, etc.).
pub fn measure_plugin(path: &Path) -> Result<(f32, f32)> {
    let stats = smoke::render_stats(path, &[], MEASURE_DURATION_SECS, 60, 100)?;
    let rms_db = if stats.rms > 0.0 {
        20.0 * stats.rms.log10()
    } else {
        f32::NEG_INFINITY
    };
    let peak_db = if stats.peak > 0.0 {
        20.0 * stats.peak.log10()
    } else {
        f32::NEG_INFINITY
    };
    Ok((rms_db, peak_db))
}

// ── entry point ────────────────────────────────────────────────────────

pub fn run(argv: &[String]) -> Result<()> {
    let mut all = false;
    let mut only: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut target: Option<PathBuf> = None;

    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--all" => all = true,
            "--only" => {
                only = Some(
                    it.next()
                    .ok_or_else(|| anyhow!("--only needs a plugin name"))?
                    .clone(),
                );
            }
            "--out" | "-o" => {
                out = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("--out needs a path"))?,
                ));
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            s if s.starts_with('-') => bail!("unknown flag: {s}"),
            s => {
                if target.is_some() {
                    bail!("measure takes at most one plugin path");
                }
                target = Some(PathBuf::from(s));
            }
        }
    }

    // Case 1: a single plugin path given directly.
    if let Some(t) = target {
        let (rms, peak) = measure_plugin(&t)?;
        println!("{}: rms {:.1} dB, peak {:.1} dB", t.display(), rms, peak);
        return Ok(());
    }

    // Case 2: --only NAME — look up installed plugin, measure it.
    let index = PluginIndex::scan();
    if let Some(name) = only {
        let Some(path) = index.find_any(&name).cloned() else {
            bail!("no plugin found matching {:?}", name);
        };
        let (rms, peak) = measure_plugin(&path)?;
        println!("{}: rms {:.1} dB, peak {:.1} dB", name, rms, peak);
        return Ok(());
    }

    // Case 3: --all — walk the registries, measure every referenced plugin.
    if all {
        let out = out.unwrap_or_else(|| {
            PathBuf::from("share").join(INVENTORY_FILENAME)
        });
        return measure_and_write(&index, &out);
    }

    bail!("measure needs a plugin path, --only NAME, or --all (try --help)");
}

fn print_help() {
    println!("usage: trackscript-player measure <plugin.vst3>");
    println!("       trackscript-player measure --only NAME");
    println!("       trackscript-player measure --all [--out FILE]");
    println!();
    println!("  Measure a plugin's default-patch output level (RMS + peak in");
    println!("  dBFS) by playing a sustained C4 at velocity 100 through it.");
    println!();
    println!("  --all  measure every plugin referenced by fallbacks.json and");
    println!("         registry.json, write share/plugins_inventory.json");
}

fn measure_and_write(index: &PluginIndex, out: &Path) -> Result<()> {
    let names = gather_names();
    if names.is_empty() {
        eprintln!(
            "no plugin names found in share/{} or share/registry.json",
            FALLBACKS_FILENAME
        );
        return Ok(());
    }

    println!("measuring {} plugin(s)...\n", names.len());

    let mut inv = PluginInventory::default();
    let mut missing = Vec::new();

    for name in &names {
        let Some(path) = index.find_any(name).cloned() else {
            missing.push(name.clone());
            continue;
        };
        match measure_plugin(&path) {
            Ok((rms, peak)) => {
                let silent = !rms.is_finite() && !peak.is_finite();
                let tag = if silent { "  (silent)" } else { "" };
                println!(
                    "  {:<30} rms {:>7.1} dB, peak {:>7.1} dB{}",
                    name, rms, peak, tag
                );
                inv.plugins.insert(
                    name.clone(),
                                   PluginLevel::from_measurement(
                                       plugin_kind(&path),
                                                                 path.display().to_string(),
                                                                 rms,
                                                                 peak,
                                   ),
                );
            }
            Err(e) => eprintln!("  {:<30} FAILED: {:#}", name, e),
        }
    }

    inv.finalize();
    inv.save(out)?;

    println!(
        "\nwrote {} plugin level(s) to {}",
             inv.plugins.len(),
             out.display()
    );
    if !missing.is_empty() {
        println!("\n{} plugin(s) referenced but not installed:", missing.len());
        for n in &missing {
            println!("  {}", n);
        }
    }
    Ok(())
}

fn gather_names() -> Vec<String> {
    use std::collections::BTreeSet;

    let mut set: BTreeSet<String> = BTreeSet::new();

    let fb_path = PathBuf::from("share").join(FALLBACKS_FILENAME);
    if let Some(fb) = FallbacksTable::load(&fb_path) {
        for chain in fb.instruments.values() {
            for c in chain {
                if !c.name.is_empty() {
                    set.insert(c.name.clone());
                }
            }
        }
        for m in &fb.effects {
            if !m.is_skip() && !m.fallback.name.is_empty() {
                set.insert(m.fallback.name.clone());
            }
        }
    }

    set.into_iter().collect()
}

fn plugin_kind(path: &Path) -> &'static str {
    match path
    .extension()
    .and_then(|s| s.to_str())
    .map(|s| s.to_ascii_lowercase())
    .as_deref()
    {
        Some("vst3") => "vst3",
        Some("lv2")  => "lv2",
        Some("clap") => "clap",
        Some("vst") | Some("so") | Some("dll") => "vst2",
        _ => "unknown",
    }
}
