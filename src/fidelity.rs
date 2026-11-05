//! FL fidelity sidecar schema.
//!
//! The .halotext is the readable arrangement.  This is everything
//! else: channel levels, envelopes, tracking, per-channel automation,
//! exact playlist placements, multi-sample zones, and mixer insert
//! chains.  Written next to the .halotext as `fidelity.json`, read
//! back by `player::load_project`.
//!
//! Numeric fields are `f64` throughout.  FL stores many of these as
//! fixed-point integers in the binary, but the parser may surface them
//! as either `i32` or `f32`.  `f64` accepts both JSON numbers, so we
//! never fail a whole object on a single type mismatch.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const FIDELITY_FILENAME: &str = "fidelity.json";
pub const FIDELITY_VERSION: u32 = 2;

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

    // ── v2 additions: fields preserved from the FLP that the parser
    //    captures raw and the converter does not yet interpret. ──
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_controllers_hex: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track_data_hex: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_events: Vec<UnknownEventFidelity>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pattern_names: BTreeMap<u16, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageFidelity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelPluginFidelity {
    #[serde(default)]
    pub internal_name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub canonical_name: String,
    #[serde(default)]
    pub vst_path: String,
    #[serde(default)]
    pub state_size: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument_path: Option<String>,
    #[serde(default)]
    pub fallback: bool,
    /// Event 212, verbatim (52 B per dawparse).  `owner` is already
    /// surfaced elsewhere; the remaining bytes hold mixer insert/slot
    /// and wrapper flags that are not yet decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapper_hex: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<ChannelPluginFidelity>,

    // ── v2 additions ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<DelayFidelity>,
    /// Event 215, verbatim (157 or 168 B depending on FL version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters_hex: Option<String>,
    /// Event 221, verbatim (9 B).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polyphony_hex: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DelayFidelity {
    #[serde(default)] pub feedback: f64,
    #[serde(default)] pub pan: f64,
    #[serde(default)] pub pitch_shift: f64,
    #[serde(default)] pub echoes: f64,
    #[serde(default)] pub time: f64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct UnknownEventFidelity {
    pub id: u8,
    #[serde(default)]
    pub count: usize,
    /// First 32 bytes hex-encoded.  Full bytes live in `Project.events`;
    /// they are dropped when `convert()` returns, so this is the only
    /// preservation for unknown IDs at the moment.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview_hex: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CoverageFidelity {
    #[serde(default)] pub total_events: usize,
    #[serde(default)] pub unique_ids: usize,
    #[serde(default)] pub handled_ids: Vec<u8>,
    #[serde(default)] pub unhandled_id_counts: BTreeMap<u8, usize>,
}

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
    #[serde(default)] pub name: String,
    #[serde(default)] pub target: String,
    #[serde(default)] pub unit: String,
    #[serde(default)] pub points: Vec<AutomationPoint>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AutomationPoint {
    #[serde(default)] pub tick: f64,
    #[serde(default)] pub value: f64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PlaylistFidelity {
    #[serde(default)] pub position: f64,
    #[serde(default)] pub length: f64,
    #[serde(default)] pub track: f64,
    #[serde(default)] pub kind: String,
    #[serde(default)] pub ref_id: f64,
    #[serde(default)] pub clip_start_ticks: f64,
    #[serde(default)] pub start_offset_ms: f64,
    #[serde(default)] pub end_offset_ms: f64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MultiSampleFidelity {
    pub files: Vec<String>,
    pub root_keys: Vec<Option<i32>>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct InsertFidelity {
    #[serde(default)] pub name: String,
    #[serde(default)] pub slots: Vec<InsertSlotFidelity>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct InsertSlotFidelity {
    #[serde(default)] pub slot: u32,
    #[serde(default)] pub plugin: String,
    #[serde(default)] pub internal_name: String,
    #[serde(default)] pub vst_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default)] pub fallback: bool,
}

impl Fidelity {
    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(text)?)
    }
}