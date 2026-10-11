// src/convert/mod.rs
//! Source → Trackscript conversion.
//!
//! One public entry point: `convert_file` takes any supported source and
//! returns a `Converted` bundle.  Nothing touches the disk except reading
//! the source.  Callers (`export`, `play`, `test`) write the bundle
//! themselves.

mod flp;
mod it;

use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::sources::trackscript;
use crate::sources::{self, SourceKind};

// ── public API ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PitchMode {
    Names,
    Hz,
}

impl Default for PitchMode {
    fn default() -> Self {
        PitchMode::Names
    }
}

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub pitch_mode: PitchMode,
    pub subdivisions: u32,
    pub skip_native: bool,
    pub samples_dir: Option<PathBuf>,
    pub with_message: bool,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        ConvertOptions {
            pitch_mode: PitchMode::Names,
            subdivisions: 4,
            skip_native: false,
            samples_dir: None,
            with_message: true,
        }
    }
}

pub struct SampleOut {
    pub rel_path: String,
    pub wav: Vec<u8>,
}

pub struct AuxFile {
    pub rel_path: String,
    pub bytes: Vec<u8>,
}

pub struct Converted {
    pub trackscript: String,
    pub samples: Vec<SampleOut>,
    pub samples_json: String,
    pub aux: Vec<AuxFile>,
}

pub fn convert_file(src: &Path, opts: &ConvertOptions) -> Result<Converted> {
    match SourceKind::from_path(src)? {
        SourceKind::Flp => {
            let prj = sources::flp::parse_file(src)?;
            flp::convert(&prj, src, opts)
        }
        SourceKind::It => {
            let module = sources::it::parse_file(src)?;
            it::convert(&module, src, opts)
        }
        SourceKind::Trackscript => {
            let text = std::fs::read_to_string(src)?;
            trackscript::parse_str(&text)?;
            Ok(Converted {
                trackscript: text,
                samples: Vec::new(),
               samples_json: String::new(),
               aux: Vec::new(),
            })
        }
    }
}

pub fn output_stem(src: &Path) -> String {
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let mut out = String::with_capacity(stem.len());
    let mut prev_us = false;
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
            out.push(c);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    let t = out.trim_matches('_');
    if t.is_empty() { "output".into() } else { t.into() }
}

pub fn default_out_dir(src: &Path) -> PathBuf {
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let dir = src.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("{stem}_trackscript"))
}

// ── shared helpers ─────────────────────────────────────────────────────

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

fn note_name(n: i32) -> String {
    let idx = n.rem_euclid(12) as usize;
    let octave = n.div_euclid(12);
    format!("{}{}", NOTE_NAMES[idx], octave)
}

fn safe_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_us = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
            out.push(c);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    let t = out.trim_matches('_');
    if t.is_empty() { "part".into() } else { t.into() }
}

fn identifier(s: &str) -> String {
    let s = safe_name(s);
    if s.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        format!("_{}", s)
    } else {
        s
    }
}

fn fmt(x: f64, nd: usize) -> String {
    let s = format!("{:.*}", nd, x);
    let t = s.trim_end_matches('0').trim_end_matches('.');
    if t.is_empty() || t == "-0" { "0".into() } else { t.into() }
}

fn pos_str(row: f64, subs: u32) -> String {
    let subs_f = subs as f64;
    let mut beat = (row / subs_f).floor() as i64 + 1;
    let sub = row - subs_f * (beat as f64 - 1.0) + 1.0;
    let sub = (sub * 100.0).round() / 100.0;
    let sub_round = sub.round();
    if (sub - sub_round).abs() < 1e-9 {
        let sub_int = sub_round as i64;
        if sub_int > subs as i64 {
            beat += 1;
            return format!("§{}/1", beat);
        }
        return format!("§{}/{}", beat, sub_int);
    }
    format!("§{}/{}", beat, fmt(sub, 2))
}

// ── samples.json ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct SamplesJson {
    format: &'static str,
        version: u32,
        samples: Vec<SampleJsonEntry>,
        instruments: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct SampleJsonEntry {
    id: u16,
    name: String,
    file: String,
    #[serde(rename = "c5Speed")]
    c5_speed: u32,
}
