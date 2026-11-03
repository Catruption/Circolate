//! Data model: events, parts, blocks, config, shims, fallbacks, lyrics.

use serde::Serialize;
use std::collections::HashMap;

// ── events ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct HalotextEvent {
    pub op: String,
    pub row: f64,
    /// One-based line number in the source Halotext file.
    pub source_line: usize,
    /// Zero-based UTF-8 byte columns for editor integrations.
    pub source_byte_column: usize,
    pub source_end_byte_column: usize,
    pub pitch_midi: Option<i32>,
    pub pitch_hz: Option<f64>,
    pub vol: Option<f64>,
    pub pan: Option<f64>,
    pub dur_beats: Option<f64>,
    pub sample_override: Option<String>,
    pub offset: Option<u32>,
    pub bank: Option<i16>,
    pub preset: Option<i16>,
    pub root_note: Option<i16>,
    pub src_bpm: Option<f64>,
    pub fine_pitch: Option<i16>,
    pub release: Option<i16>,
    pub mod_x: Option<i16>,
    pub mod_y: Option<i16>,
    pub lyric: Option<String>,
    pub raw: String,
}

impl HalotextEvent {
    pub fn is_waypoint(&self) -> bool {
        self.op == crate::spec::EV_WAYPOINT
    }
}

// ── parts ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct HalotextPart {
    pub name: String,
    pub sample_path: String,
    pub beats_per_measure: u32,
    pub subs_per_beat: u32,
    pub len_beats: Option<f64>,
    pub sample_offset: u32,
    pub preset: Option<i16>,
    pub bank: Option<i16>,
    pub src_bpm: Option<f64>,
    pub events: Vec<HalotextEvent>,
    pub measures: Vec<Vec<HalotextEvent>>,
    pub section_lengths: Vec<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_vol: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_pan: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_dur: Option<f64>,
}

impl HalotextPart {
    /// Composite SF2 lookup key.  Returns `sample_path` unchanged for
    /// non-SF2 parts; appends `#preset` (or `#bank:preset` for nonzero
    /// banks) for `.sf2` parts so multiple parts sharing one soundfont
    /// but different presets resolve to distinct instruments.
    ///
    /// The format here must match the key construction in
    /// `player::project::loader` (which inserts these keys) and
    /// `player::template` (which reads them).
    pub fn sf2_key(&self) -> String {
        if !self.sample_path.ends_with(".sf2") {
            return self.sample_path.clone();
        }
        let p = self.preset.unwrap_or(0);
        let b = self.bank.unwrap_or(0);
        if b == 0 {
            format!("{}#{}", self.sample_path, p)
        } else {
            format!("{}#{}:{}", self.sample_path, b, p)
        }
    }
}

// ── shims ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ShimParam {
    pub slot: u32,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum ShimField {
    Mix,
    Gain,
    Pan,
    Pitch,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShimWaypoint {
    pub row: f64,
    pub field: ShimField,
    pub value: f64,
    pub ramp_rows: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShimEvent {
    pub line: usize,
    pub row: f64,
    pub target: String,
    pub params: Vec<ShimParam>,
    pub length_rows: Option<f64>,
    pub mix: f64,
    pub gain: f64,
    pub pan: f64,
    pub attached_call: Option<usize>,
    pub waypoints: Vec<ShimWaypoint>,
    // Property-shim fields for `ƒTempo` and future `ƒGlobal*`:
    pub property: String,
    pub start_value: f64,
    pub op: String,
    pub amount: f64,
    pub interval_rows: f64,
    pub count: Option<u32>,
}

impl ShimEvent {
    /// Effective start row of the shim's window.  For call-attached shims
    /// the row is 0 until the BlockList cursor is resolved by the loader.
    pub fn effective_row(&self) -> f64 {
        self.row
    }
}

// ── calls and overrides ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct BlockCall {
    pub name: String,
    pub selections: Vec<MeasureSelection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MeasureSelection {
    pub measure: usize,
    pub overrides: Option<CallOverrides>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct CallOverrides {
    pub vol: Option<f64>,
    pub pan: Option<f64>,
    pub dur: Option<f64>,
    pub source: Option<String>,
}

// ── notes ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct NoteEvent {
    pub row: f64,
    pub pitch: i32,
    pub pitch_hz: f64,
    pub voice: String,
    pub sample: Option<String>,
    pub duration_rows: Option<f64>,
    pub duration_beats: Option<f64>,
    pub section: usize,
    pub source: String,
}

// ── config ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct Config {
    pub bpm: f64,
    pub subdivisions: u32,
    pub time_signature: (u32, u32),
    pub song_volume: f64,
    pub panning_law: String,
    pub stereo_width: f32, // <--- NEW
    pub raw: HashMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bpm: 120.0,
            subdivisions: 4,
            time_signature: (4, 4),
            song_volume: 100.0,
            panning_law: "equal_power".into(),
            stereo_width: 1.0, // <--- NEW
            raw: HashMap::new(),
        }
    }
}

// ── lyrics ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct LyricSegment {
    pub row: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LyricEvent {
    pub start_row: f64,
    pub end_row: f64,
    pub segments: Vec<LyricSegment>,
}

// ── fallbacks ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FallbackSpec {
    #[serde(rename = "use")]
    pub use_: String,
    pub gain: Option<f64>,
}

// ── top-level file ─────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize)]
#[allow(dead_code)]
pub struct HalotextFile {
    pub config: Config,
    pub tempo_events: Vec<(f64, f64, f64)>,
    pub block_list: Vec<Vec<BlockCall>>,
    pub shims: Vec<ShimEvent>,
    pub blocks: HashMap<String, Vec<String>>,
    pub parts: HashMap<String, HalotextPart>,
    pub notes: Vec<NoteEvent>,
    pub lyrics: Vec<LyricEvent>,
    pub fallbacks: HashMap<String, FallbackSpec>,
}
