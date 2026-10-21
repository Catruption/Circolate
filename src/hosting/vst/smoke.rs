//! `trackscript-player vst-smoke <plugin.vst3>` — load one VST3, send a
//! note, render two seconds, report peak level.  Smoke test for the shim.
//!
//! Also exposes `render_stats()`, used by `measure.rs`. The point of
//! sharing is consistency: if `vst-smoke` says a plugin plays,
//! `measure` must agree, and vice versa. Two separate render loops
//! inevitably diverge — the previous version of `measure` used
//! `process_inplace()` (the *effect* entry point) on instrument plugins,
//! so every synth measured `-inf` while `vst-smoke` saw audio.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

use crate::hosting::vst::ffi::VstHost;

const SR: u32 = 44100;
const BLOCK: usize = 512;

#[derive(Debug, Clone, Copy)]
pub struct RenderStats {
    pub peak: f32,
    pub rms: f32,
    pub duration_secs: f32,
}

pub fn render_stats(
    path: &Path,
    state: &[u8],
    duration_secs: f32,
    note: u8,
    velocity: u8,
) -> Result<RenderStats> {
    let path_str = path.to_str()
    .ok_or_else(|| anyhow::anyhow!("non-UTF8 plugin path"))?;
    let mut host = VstHost::load(path_str, state, SR)?;

    let total = (SR as f32 * duration_secs) as usize;
    let mut l = vec![0f32; total];
    let mut r = vec![0f32; total];

    host.note_on(note, velocity);
    let blocks = total / BLOCK;
    let release_block = blocks / 2;
    for b in 0..blocks {
        if b == release_block {
            host.note_off(note);
        }
        let off = b * BLOCK;
        host.process(&mut l[off..off + BLOCK], &mut r[off..off + BLOCK]);
    }

    let peak = l.iter().chain(r.iter()).fold(0f32, |a, &b| a.max(b.abs()));
    let rms = (l.iter().chain(r.iter()).map(|&s| s * s).sum::<f32>()
    / (2 * total) as f32).sqrt();

    Ok(RenderStats { peak, rms, duration_secs })
}

pub fn run(argv: &[String]) -> Result<()> {
    let dump_params = argv.iter().any(|a| a == "--dump-params");
    let args: Vec<&String> = argv.iter().filter(|a| *a != "--dump-params").collect();

    let Some(path) = args.first() else {
        bail!(
            "usage: trackscript-player vst-smoke <plugin.vst3> [state.file] [--dump-params]"
        );
    };
    let path = PathBuf::from(path.as_str());

    if dump_params {
        let path_str = path.to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF8 plugin path"))?;
        let mut host = VstHost::load(path_str, &[], SR)?;
        eprintln!("parameters of {} (slot N is used as `αN` / `βN` / … in the trackscript shim):", path.display());
        let n = host.dump_params();
        if n < 0 {
            eprintln!("(plugin has no IEditController; no parameters)");
        } else {
            eprintln!("{} parameter(s)", n);
        }
        return Ok(());
    }

    let state_path: Option<PathBuf> = args.get(1).map(|s| PathBuf::from(s.as_str()));
    let state: Vec<u8> = match &state_path {
        Some(p) => std::fs::read(p)?,
        None    => Vec::new(),
    };

    if let Some(sp) = &state_path {
        if let Some(root) = sp.parent().and_then(|p| p.parent()) {
            if root.is_dir() {
                std::env::set_current_dir(root)?;
            }
        }
    }

    println!("loading {}", path.display());
    let stats = render_stats(&path, &state, 2.0, 60, 100)?;

    println!("peak: {:.4}   rms: {:.4}", stats.peak, stats.rms);
    if stats.peak < 1e-4 {
        eprintln!("SILENT — plugin loaded but produced no audio");
        eprintln!("check: bus activation, class selection, state format");
    } else {
        println!("ok");
    }
    Ok(())
}
