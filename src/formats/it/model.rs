//! IT — parsed data structures.

use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteTermination {
    Fade,
    Cut,
    KeyOff,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ITEnvelope {
    pub enabled: bool,
    pub loop_enabled: bool,
    pub sustain_enabled: bool,
    pub loop_start: u8,
    pub loop_end: u8,
    pub sustain_start: u8,
    pub sustain_end: u8,
    /// (tick, value) pairs. Value is signed (-128..127).
    pub points: Vec<(u16, i8)>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Instrument {
    pub name: String,
    pub nna: u8,
    pub dct: u8,
    pub dca: u8,
    pub fadeout: u16,
    pub gbv: u8,
    pub dfp: u8,
    /// (note, sample) pairs. Note 0..119, sample 0..255 (0 = none).
    pub keymap: Vec<(u8, u8)>,
    /// Envelope enabled flags, keyed by "volume" / "pan" / "pitch".
    pub env: BTreeMap<String, bool>,
    pub envelopes: BTreeMap<String, ITEnvelope>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Sample {
    pub name: String,
    pub gvl: u8,
    pub flg: u8,
    pub vol: u8,
    pub cvt: u8,
    pub dfp: u8,
    pub length: u32,
    pub loop_beg: u32,
    pub loop_end: u32,
    pub c5: u32,
    pub sus_beg: u32,
    pub sus_end: u32,
    pub ptr: u32,
    pub has_data: bool,
    pub is16: bool,
    pub stereo: bool,
    pub compressed: bool,
    pub r#loop: bool,
    pub pingpong: bool,
    pub sus_loop: bool,
    pub pingpong_sus: bool,
    /// Decoded PCM, signed. i32 because 16-bit samples need the range.
    #[serde(skip)]
    pub pcm: Vec<i32>,
    /// Filled in by the converter; None during parse.
    pub file: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatternEvent {
    pub note: Option<u8>,
    pub instrument: Option<u8>,
    pub volume: Option<u8>,
    pub command: Option<u8>,
    pub parameter: u8,
}

impl PatternEvent {
    pub fn is_empty(&self) -> bool {
        self.note.is_none()
        && self.instrument.is_none()
        && self.volume.is_none()
        && self.command.is_none()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Module {
    pub title: String,
    pub cwtv: u16,
    pub cmwt: u16,
    pub highlight_minor: u8,
    pub highlight_major: u8,
    pub gv: u8,
    pub mv: u8,
    pub speed: u8,
    pub tempo: u8,
    pub sep: u8,
    pub pwd: u8,
    pub flags: u16,
    pub special: u16,
    pub stereo: bool,
    pub use_instruments: bool,
    pub linear: bool,
    pub old_effects: bool,
    pub chnpan: Vec<u8>,
    pub chnvol: Vec<u8>,
    pub orders: Vec<u8>,
    pub message: String,
    pub instruments: Vec<Instrument>,
    pub samples: Vec<Sample>,
    pub patterns: Vec<Vec<Vec<Option<PatternEvent>>>>,
}