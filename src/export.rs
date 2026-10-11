//! `trackscript-player export` — convert a source file to trackscript.

use anyhow::{anyhow, bail, Result};
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

    let ts = convert::export(&src, &out, &opts)?;
    println!("wrote {}", ts.display());
    Ok(())
}
