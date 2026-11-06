//! Shared conversion contracts and formatting helpers.
//!
//! Format converters depend on these types instead of the application's
//! dispatcher, keeping format packages independent of dispatch policy.

use serde::Serialize;
use std::path::PathBuf;

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
    pub sf2_path: Option<PathBuf>,   // ← new
    pub with_message: bool,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        ConvertOptions {
            pitch_mode: PitchMode::Names,
            subdivisions: 4,
            skip_native: false,
            samples_dir: None,
            sf2_path: None,
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
    pub halotext: String,
    pub samples: Vec<SampleOut>,
    pub samples_json: String,
    pub aux: Vec<AuxFile>,
}

// ── shared helpers ─────────────────────────────────────────────────────

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

pub fn note_name(n: i32) -> String {
    let idx = n.rem_euclid(12) as usize;
    let octave = n.div_euclid(12);
    format!("{}{}", NOTE_NAMES[idx], octave)
}

pub fn safe_name(s: &str) -> String {
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

pub fn identifier(s: &str) -> String {
    let s = safe_name(s);
    if s.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        format!("_{}", s)
    } else {
        s
    }
}

pub fn fmt(x: f64, nd: usize) -> String {
    let s = format!("{:.*}", nd, x);
    let t = s.trim_end_matches('0').trim_end_matches('.');
    if t.is_empty() || t == "-0" { "0".into() } else { t.into() }
}

pub fn pos_str(row: f64, subs: u32) -> String {
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
pub struct SamplesJson {
    pub format: &'static str,
    pub version: u32,
    pub samples: Vec<SampleJsonEntry>,
    pub instruments: Vec<serde_json::Value>,
}

#[derive(Serialize)]
pub struct SampleJsonEntry {
    pub id: u16,
    pub name: String,
    pub file: String,
    #[serde(rename = "c5Speed")]
    pub c5_speed: u32,
}
