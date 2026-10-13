// src/convert/it.rs
//! IT → Trackscript converter.
//!
//! Direct port of it2trackscript.py.  Runs the IT playback state machine
//! row by row, produces a list of voices with pitch/volume/pan automation,
//! then groups them into measure-sized sections and emits trackscript via
//! the shared `Song` writer.
//!
//! Submodules:
//!   * `helpers`  — small numeric/text utilities shared by the rest
//!   * `sim`      — Voice, ChannelState, Converter + the IT state machine
//!   * `render`   — voices → per-measure sections; the `Song` builder
//!   * `report`   — human-readable report + semantics JSON
//!   * `wav`      — WAV encoding + samples.json

mod helpers;
mod render;
mod report;
mod sim;
mod wav;

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::{AuxFile, ConvertOptions, Converted, SampleOut};
use crate::sources::it::Module;

use helpers::safe_name;
use render::build_song;
use sim::Converter;
use wav::{build_samples_json, write_wav};

// ── entry point ────────────────────────────────────────────────────────

pub(super) fn convert(
    module: &Module,
    src_path: &Path,
    opts: &ConvertOptions,
) -> Result<Converted> {
    let base = src_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "song".into());
    let source_name = src_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| base.clone());

    // Samples: decode PCM into WAV bytes, in memory.
    let mut sample_files: HashMap<u32, String> = HashMap::new();
    let mut sample_outs: Vec<SampleOut> = Vec::new();

    for (i, s) in module.samples.iter().enumerate() {
        let idx = i as u32 + 1;
        if !s.pcm.is_empty() {
            let fname = format!(
                "{:02}_{}.wav",
                idx,
                safe_name(if s.name.is_empty() { "sample" } else { &s.name })
            );
            let rel = format!("samples/{}", fname);
            sample_files.insert(idx, rel.clone());
            sample_outs.push(SampleOut {
                rel_path: rel,
                wav: write_wav(s),
            });
        }
    }

    let samples_json = build_samples_json(module, &sample_files)?;

    let mut conv = Converter::new(module, opts.pitch_mode);
    conv.run();

    let song = build_song(
        &mut conv,
        &base,
        &sample_files,
        &source_name,
        opts.with_message,
    );
    let trackscript = song.write();

    // The report and semantics JSON are computed for their side effects
    // (warnings fire, statistics gather) but not written here; a future
    // pass can push them into `aux` if desired.
    let _ = report::make_report(&mut conv, &source_name, &sample_files);
    let _ = report::build_semantics_json(module, &conv, &source_name)?;

    // Byte-exact copy of the source, kept for unsupported/reserved data.
    let source_bytes = fs::read(src_path)
        .with_context(|| format!("reading {}", src_path.display()))?;
    let aux = vec![AuxFile {
        rel_path: format!("source/{}", source_name),
        bytes: source_bytes,
    }];

    Ok(Converted {
        trackscript,
        samples: sample_outs,
        samples_json,
        aux,
    })
}