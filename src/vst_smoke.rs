//! `trackscript-player vst-smoke <plugin.vst3>` — load one VST3, send a
//! note, render two seconds, report peak level.  Smoke test for the shim.

use anyhow::{bail, Result};

use crate::vst_host_ffi::VstHost;

const SR: u32 = 44100;
const BLOCK: usize = 512;
const SECONDS: usize = 2;

pub fn run(argv: &[String]) -> Result<()> {
    let Some(path) = argv.first() else {
        bail!("usage: trackscript-player vst-smoke <plugin.vst3> [state.file]");
    };
    let state: Vec<u8> = match argv.get(1) {
        Some(p) => std::fs::read(p)?,
        None    => Vec::new(),
    };

    println!("loading {path}");
    let mut host = VstHost::load(path, &state, SR)?;

    let total = SR as usize * SECONDS;
    let mut l = vec![0f32; total];
    let mut r = vec![0f32; total];

    host.note_on(60, 100);
    let blocks = total / BLOCK;
    for b in 0..blocks {
        // Release the note around the one-second mark.
        if b == SR as usize / BLOCK {
            host.note_off(60);
        }
        let off = b * BLOCK;
        host.process(&mut l[off..off + BLOCK], &mut r[off..off + BLOCK]);
    }

    let peak = l
    .iter()
    .chain(r.iter())
    .fold(0f32, |a, &b| a.max(b.abs()));
    let rms = (l.iter().chain(r.iter()).map(|&s| s * s).sum::<f32>()
    / (2 * total) as f32)
    .sqrt();

    println!("peak: {peak:.4}   rms: {rms:.4}");
    if peak < 1e-4 {
        eprintln!("SILENT — plugin loaded but produced no audio");
        eprintln!("check: bus activation, class selection, state format");
    } else {
        println!("ok");
    }
    Ok(())
}
