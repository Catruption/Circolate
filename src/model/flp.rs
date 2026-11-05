//! FL Studio project — pure data structures.
//!
//! No I/O, no parsing, no conversion. Just the shape of what the parser
//! produces and what the pipe consumes. Accessors here operate only on
//! the struct's own fields.

use serde::Serialize;
use std::collections::BTreeMap;

// ── channel substructures ─────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize)]
pub struct EnvelopeLfo {
    pub flags: i32,
    pub env_enabled: i32,
    pub env_predelay: i32,
    pub env_attack: i32,
    pub env_hold: i32,
    pub env_decay: i32,
    pub env_sustain: i32,
    pub env_release: i32,
    pub env_amount: i32,
    pub lfo_predelay: u32,
    pub lfo_attack: u32,
    pub lfo_amount: i32,
    pub lfo_speed: u32,
    pub lfo_shape: i32,
    pub env_attack_tension: i32,
    pub env_decay_tension: i32,
    pub env_release_tension: i32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Levels {
    pub pan: i32,
    pub volume: u32,
    pub pitch_shift: i32,
    pub filter_mod_x: u32,
    pub filter_mod_y: u32,
    pub filter_type: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Tracking {
    pub middle_value: u32,
    pub pan: i32,
    pub mod_x: i32,
    pub mod_y: i32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LevelAdjusts {
    pub pan: i32,
    pub volume: u32,
    pub mod_x: i32,
    pub mod_y: i32,
}

/// `ChannelDelay` (event 209). 20 bytes, five signed 32-bit fields.
/// Field names from pyflp; order inferred from pyflp's struct declaration
/// and pyflp's binary dump. Confidence: medium — verify against a
/// rendered FLP before relying on the individual fields.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ChannelDelay {
    pub feedback: i32,
    pub pan: i32,
    pub pitch_shift: i32,
    pub echoes: i32,
    pub time: i32,
}

/// Payload we know the ID and length of but do not decode. Preserved
/// verbatim so a future converter can grow without re-reading the file.
#[derive(Debug, Clone, Serialize)]
pub struct RawEvent {
    pub id: u8,
    pub len: usize,
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

// ── channel-rack entities ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub position: u32,
    pub length: u32,
    pub key: u16,
    pub channel: u16,
    pub velocity: u8,
    pub pan: u8,
    pub fine_pitch: u8,
    pub release: u8,
    pub mod_x: u8,
    pub mod_y: u8,
    pub flags: u16,
    pub group: u16,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Plugin {
    pub owner: u32,
    pub internal_name: String,
    pub display_name: String,
    pub vst_path: String,
    pub state_size: usize,
    /// Raw bytes of event 212 (52 bytes per dawparse KNOWN_SIZES).
    /// The first 4 bytes are `owner`; the remaining 48 are not yet
    /// interpreted but are preserved so the wrapper can be re-decoded
    /// without touching the original FLP.
    #[serde(skip)]
    pub wrapper_bytes: Vec<u8>,
    #[serde(skip)]
    pub state: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Channel {
    pub iid: u16,
    pub type_id: i16,
    #[serde(rename = "type")]
    pub channel_type: String,
    pub name: String,
    pub color: Option<u32>,
    pub sample_path: String,
    pub plugin: Option<Plugin>,

    /// Mixer insert the channel feeds. 0 = master, 1..=127 = user insert.
    pub insert: u16,

    pub enabled: Option<bool>,
    pub levels: Option<Levels>,
    pub envelopes: Vec<EnvelopeLfo>,
    pub tracking: Vec<Tracking>,
    pub level_adjusts: Option<LevelAdjusts>,
    pub sampler_flags: Option<u32>,
    pub sample_start: Option<u32>,
    pub sample_end: Option<u32>,
    pub root_note: Option<u32>,
    pub fine_tune: Option<i32>,
    pub delay: Option<ChannelDelay>,

    /// Event 215. Raw bytes (157 or 168 depending on FL version).
    #[serde(skip)]
    pub parameters: Option<RawEvent>,
    /// Event 221. Raw bytes (9).
    #[serde(skip)]
    pub polyphony: Option<RawEvent>,
}

impl Channel {
    /// Blank-slate channel. `parse()` fills in type/name/etc. as it
    /// walks the event stream.
    pub fn new(iid: u16) -> Self {
        Channel {
            iid,
            type_id: -1,
            channel_type: "?".into(),
            name: String::new(),
            color: None,
            sample_path: String::new(),
            plugin: None,
            insert: 0,
            enabled: None,
            levels: None,
            envelopes: Vec::new(),
            tracking: Vec::new(),
            level_adjusts: None,
            sampler_flags: None,
            sample_start: None,
            sample_end: None,
            root_note: None,
            fine_tune: None,
            delay: None,
            parameters: None,
            polyphony: None,
        }
    }

    pub fn envelope(&self, slot: usize) -> Option<&EnvelopeLfo> {
        self.envelopes.get(slot)
    }

    pub fn pan_norm(&self) -> f64 {
        if let Some(l) = &self.levels {
            return ((l.pan as f64 - 6400.0) / 6400.0).clamp(-1.0, 1.0);
        }
        if let Some(l) = &self.level_adjusts {
            return (l.pan as f64 / 12800.0).clamp(-1.0, 1.0);
        }
        0.0
    }

    pub fn volume_scale(&self) -> f64 {
        if let Some(l) = &self.levels {
            if l.volume == 0 {
                return 0.0;
            }
            return (l.volume as f64 / 12800.0).clamp(0.0, 2.0);
        }
        if let Some(l) = &self.level_adjusts {
            if l.volume == 0 {
                return 0.0;
            }
            return (l.volume as f64 / 12800.0).clamp(0.0, 2.0);
        }
        1.0
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Pattern {
    pub iid: u16,
    pub name: String,
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaylistItem {
    pub position: u32,
    pub length: u32,
    pub track: i32,
    pub kind: String,
    #[serde(rename = "ref")]
    pub ref_id: u16,
    pub flags: u16,
    pub start_offset_ms: f32,
    pub end_offset_ms: f32,
    pub clip_start_ticks: u32,
    pub fade_in_ms: f32,
    pub fade_out_ms: f32,
    pub fade_in_curve: f32,
    pub fade_out_curve: f32,
    #[serde(skip)]
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Insert {
    pub index: u32,
    pub name: String,
    pub plugins: Vec<Plugin>,
}

// ── project ──────────────────────────────────────────────────────────

/// What the parser saw versus what it understood. Written into every
/// `Project` so downstream tooling can audit conversion completeness
/// without comparing byte lengths.
#[derive(Debug, Default, Serialize)]
pub struct EventCoverage {
    pub total_events: usize,
    pub unique_ids: usize,
    pub handled_ids: Vec<u8>,
    pub unhandled_id_counts: BTreeMap<u8, usize>,
}

#[derive(Debug, Default, Serialize)]
pub struct Project {
    pub version: String,
    pub ppq: u16,
    pub tempo: f64,
    pub title: String,
    pub author: String,
    pub genre: String,
    pub comment: String,
    pub data_path: String,
    pub channels: Vec<Channel>,
    pub patterns: Vec<Pattern>,
    pub inserts: BTreeMap<u32, Insert>,
    pub playlist: Vec<PlaylistItem>,
    pub samples: Vec<String>,
    pub coverage: EventCoverage,
    #[serde(skip)]
    pub unknown_events: Vec<RawEvent>,
    #[serde(skip)]
    pub remote_controllers: Vec<RawEvent>,
    #[serde(skip)]
    pub track_data: Vec<RawEvent>,
    #[serde(skip)]
    pub events: Vec<(u8, Vec<u8>)>,
    #[serde(default)]
    pub parse_warnings: Vec<String>,
}