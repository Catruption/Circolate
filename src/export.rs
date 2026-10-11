//! `trackscript-player export` — convert a source file to trackscript.
//!
//! Same pipeline as `play` uses: `convert_file` produces a `Converted`,
//! and we write the bundle (trackscript + samples + aux + samples.json)
//! to the output directory ourselves.  No separate exporter path to
//! drift out of sync.

use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;

use crate::convert::{self, ConvertOptions, PitchMode};

pub fn run(argv: &[String]) -> Result<()> {
    let mut src: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut opts = ConvertOptions::default();

    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--out" => {
                out_dir = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("{a} needs a path"))?,
                ));
            }
            "--pitch" => {
                let v = it
                .next()
                .ok_or_else(|| anyhow!("--pitch needs a value"))?;
                opts.pitch_mode = match v.as_str() {
                    "names" => PitchMode::Names,
                    "hz" => PitchMode::Hz,
                    other => bail!("unknown pitch mode: {other}"),
                };
            }
            "--subdivisions" => {
                opts.subdivisions = it
                .next()
                .ok_or_else(|| anyhow!("--subdivisions needs a value"))?
                .parse()?;
            }
            "--samples-dir" => {
                opts.samples_dir = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("--samples-dir needs a path"))?,
                ));
            }
            "--skip-native" => opts.skip_native = true,
            "--no-message" => opts.with_message = false,
            _ if src.is_none() => src = Some(PathBuf::from(a)),
            _ => bail!("unexpected argument: {a}"),
        }
    }
    let src = src.ok_or_else(|| {
        anyhow!("usage: trackscript-player export <file> [-o DIR] [--pitch names|hz]")
    })?;
    let out = out_dir.unwrap_or_else(|| convert::default_out_dir(&src));

    std::fs::create_dir_all(&out)
    .with_context(|| format!("creating {}", out.display()))?;

    let conv = convert::convert_file(&src, &opts)?;
    let stem = convert::output_stem(&src);

    // The trackscript itself.
    let ts_path = out.join(format!("{}.trackscript", stem));
    std::fs::write(&ts_path, &conv.trackscript)
    .with_context(|| format!("writing {}", ts_path.display()))?;

    // Decoded sample WAVs (samples/…).
    for s in &conv.samples {
        let p = out.join(&s.rel_path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, &s.wav)
        .with_context(|| format!("writing {}", p.display()))?;
    }

    // Aux files — instruments/*.instrument, instruments/*.state,
    // samples/*.sf2.
    for a in &conv.aux {
        let p = out.join(&a.rel_path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, &a.bytes)
        .with_context(|| format!("writing {}", p.display()))?;
    }

    // Sample library index.
    if !conv.samples_json.is_empty() {
        std::fs::write(out.join("samples.json"), &conv.samples_json)
        .with_context(|| format!("writing {}", out.join("samples.json").display()))?;
    }

    // Print the same footer the play path prints, so the user sees the
    // warnings and skipped-channel summary without having to cat the file.
    let mut in_footer = false;
    for line in conv.trackscript.lines() {
        if line.starts_with("// ------") {
            in_footer = true;
        } else if in_footer && line.starts_with("//") {
            println!("  {}", line);
        }
    }

    println!("wrote {}", ts_path.display());
    Ok(())
}
