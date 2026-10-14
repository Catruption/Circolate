//! `trackscript-player sf2-smoke <file.sf2>` — load an SF2 through the
//! native engine, play a note, dump a WAV.  Proves the engine works
//! without any VST plumbing.

use anyhow::{bail, Result};
use std::path::Path;

use crate::sf2_engine::Sf2Engine;

const SR: u32 = 44100;
const SECS: usize = 2;
const BLOCK: usize = 512;

pub fn run(argv: &[String]) -> Result<()> {
    let Some(path) = argv.first() else {
        bail!(
            "usage: trackscript-player sf2-smoke <file.sf2> \
[--bank N] [--patch N] [--note N] [--list] [--out FILE.wav]"
        );
    };

    let mut bank: u8 = 0;
    let mut patch: u8 = 0;
    let mut note: u8 = 60;
    let mut out: Option<String> = None;
    let mut list = false;

    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--bank"  => { bank  = argv[i + 1].parse()?; i += 2; }
            "--patch" => { patch = argv[i + 1].parse()?; i += 2; }
            "--note"  => { note  = argv[i + 1].parse()?; i += 2; }
            "--out"   => { out = Some(argv[i + 1].clone()); i += 2; }
            "--list"  => { list = true; i += 1; }
            other => bail!("unknown flag: {other}"),
        }
    }

    println!("loading {path}");
    let mut eng = Sf2Engine::new(SR);
    eng.load(Path::new(path))?;

    let presets = eng.list_presets();
    println!("{} preset(s)", presets.len());
    if list {
        for (b, p, name) in &presets {
            println!("  bank {:>3} patch {:>3} : {}", b, p, name);
        }
        return Ok(());
    }
    for (b, p, name) in presets.iter().take(8) {
        println!("  bank {} patch {} : {}", b, p, name);
    }
    if presets.len() > 8 {
        println!("  ... (--list for all)");
    }

    eng.set_preset(bank, patch);
    if !presets.iter().any(|(b, p, _)| *b == bank as u16 && *p == patch as u16) {
        eprintln!("warning: bank {} patch {} not found; will be silent", bank, patch);
    }

    let total = SR as usize * SECS;
    let mut l = vec![0.0f32; total];
    let mut r = vec![0.0f32; total];

    eng.note_on(note, 100);

    let mut b = 0usize;
    while b + BLOCK <= total {
        if b == SR as usize / 2 {
            eng.note_off(note);
        }
        eng.process(&mut l[b..b + BLOCK], &mut r[b..b + BLOCK]);
        b += BLOCK;
    }

    let peak = l.iter().chain(r.iter()).fold(0f32, |a, x| a.max(x.abs()));
    let rms = (l
    .iter()
    .chain(r.iter())
    .map(|x| x * x)
    .sum::<f32>()
    / (2 * total) as f32)
    .sqrt();
    println!("peak: {peak:.4}   rms: {rms:.4}");
    if peak < 1e-4 {
        eprintln!("SILENT — engine loaded but produced no audio");
        eprintln!("try a different --bank / --patch, or check the SF2 itself");
    }

    if let Some(out_path) = out {
        write_wav(&out_path, &l, &r, SR)?;
        println!("wrote {out_path}");
    }

    Ok(())
}

fn write_wav(path: &str, l: &[f32], r: &[f32], sr: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for (a, b) in l.iter().zip(r.iter()) {
        w.write_sample(*a)?;
        w.write_sample(*b)?;
    }
    w.finalize()?;
    Ok(())
}
