//! FL fidelity sidecar schema.
//!
//! The .trackscript is the readable arrangement.  This is everything
//! else: channel levels, envelopes, tracking, per-channel automation,
//! exact playlist placements, multi-sample zones, and mixer insert
//! chains.  Written next to the .trackscript as `fidelity.json`, read
//! back by `player::load_project`.
//!
//! Numeric fields are `f64` throughout.  FL stores many of these as
//! fixed-point integers in the binary, but the parser may surface them
//! as either `i32` or `f32`.  `f64` accepts both JSON numbers, so we
//! never fail a whole object on a single type mismatch.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const FIDELITY_FILENAME: &str = "fidelity.json";
pub const FIDELITY_VERSION: u32 = 1;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Fidelity {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub channels: BTreeMap<String, ChannelFidelity>,
    #[serde(default)]
    pub channel_automation: BTreeMap<String, AutomationFidelity>,
    #[serde(default)]
    pub playlist: Vec<PlaylistFidelity>,
    #[serde(default)]
    pub multi_sample: BTreeMap<String, MultiSampleFidelity>,
    #[serde(default)]
    pub inserts: BTreeMap<String, InsertFidelity>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ChannelFidelity {
    #[serde(default)]
    pub iid: u16,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default)]
    pub channel_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sample_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub levels: Option<LevelsFidelity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level_adjusts: Option<LevelsFidelity>,
    #[serde(default)]
    pub envelopes: Vec<EnvelopeFidelity>,
    #[serde(default)]
    pub tracking: Vec<TrackingFidelity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampler_flags: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_note: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fine_tune: Option<f64>,
}

/// Superset of FL's `levels` and `level_adjusts` shapes.  Fields absent
/// from the source object default to zero.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LevelsFidelity {
    #[serde(default)] pub pan: f64,
    #[serde(default)] pub volume: f64,
    #[serde(default)] pub pitch_shift: f64,
    #[serde(default)] pub filter_mod_x: f64,
    #[serde(default)] pub filter_mod_y: f64,
    #[serde(default)] pub filter_type: f64,
    #[serde(default)] pub mod_x: f64,
    #[serde(default)] pub mod_y: f64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct EnvelopeFidelity {
    #[serde(default)] pub flags: f64,
    #[serde(default)] pub env_enabled: f64,
    #[serde(default)] pub env_predelay: f64,
    #[serde(default)] pub env_attack: f64,
    #[serde(default)] pub env_hold: f64,
    #[serde(default)] pub env_decay: f64,
    #[serde(default)] pub env_sustain: f64,
    #[serde(default)] pub env_release: f64,
    #[serde(default)] pub env_amount: f64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TrackingFidelity {
    #[serde(default)] pub middle_value: f64,
    #[serde(default)] pub pan: f64,
    #[serde(default)] pub mod_x: f64,
    #[serde(default)] pub mod_y: f64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AutomationFidelity {
    #[serde(default)]
    pub name: String,
    /// "volume", "pan", "pitch", "filter_cutoff", "mod_x", "mod_y", ...
    #[serde(default)]
    pub target: String,
    /// "percent", "pan", "hz", "cents", ...
    #[serde(default)]
    pub unit: String,
    /// Absolute ticks within the song.
    #[serde(default)]
    pub points: Vec<AutomationPoint>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AutomationPoint {
    #[serde(default)]
    pub tick: f64,
    #[serde(default)]
    pub value: f64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PlaylistFidelity {
    #[serde(default)]
    pub position: f64,
    #[serde(default)]
    pub length: f64,
    #[serde(default)]
    pub track: f64,
    /// "pattern", "channel", "automation"
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub ref_id: f64,
    #[serde(default)]
    pub clip_start_ticks: f64,
    #[serde(default)]
    pub start_offset_ms: f64,
    #[serde(default)]
    pub end_offset_ms: f64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MultiSampleFidelity {
    /// Paths relative to the .trackscript directory.
    pub files: Vec<String>,
    /// Root MIDI note per file.  `None` = player default (60).
    pub root_keys: Vec<Option<i32>>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct InsertFidelity {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub slots: Vec<InsertSlotFidelity>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct InsertSlotFidelity {
    #[serde(default)]
    pub slot: u32,
    #[serde(default)]
    pub plugin: String,
    #[serde(default)]
    pub internal_name: String,
    #[serde(default)]
    pub vst_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default)]
    pub fallback: bool,
}

impl Fidelity {
    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(text)?)
    }
}