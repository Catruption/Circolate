//! Serde types for `share/registry.json` and `~/.config/circolate/registry.json`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Registry {
    #[serde(default = "one")]
    pub version: u32,

    #[serde(default)]
    pub instruments: Vec<Mapping>,
    #[serde(default)]
    pub effects: Vec<Mapping>,

    #[serde(default)]
    pub default_instrument: Option<FallbackTarget>,
    #[serde(default)]
    pub default_effect: Option<FallbackTarget>,

    #[serde(default)]
    pub master: Vec<MasterEntry>,

    #[serde(default)]
    pub search_paths: SearchPaths,

    #[serde(default)]
    pub on_missing: MissingPolicy,
}

fn one() -> u32 { 1 }

impl Default for Registry {
    fn default() -> Self {
        Registry {
            version: 1,
            instruments: Vec::new(),
            effects: Vec::new(),
            default_instrument: None,
                default_effect: None,
                    master: Vec::new(),
                    search_paths: SearchPaths::default(),
                    on_missing: MissingPolicy::Warn,
        }
    }
}

// ── matching ───────────────────────────────────────────────────────────

/// `match` may be either a flat string (`"Serum"`) or a structured object
/// (`{"vst_basename": "Zenology.vst", "display_name": "ZENOLOGY"}`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum MatchField {
    Flat(String),
    Structured(MatchRule),
}

impl Default for MatchField {
    fn default() -> Self { MatchField::Flat(String::new()) }
}

impl MatchField {
    /// Every string this rule wants to match against.
    pub fn candidates(&self) -> Vec<&str> {
        match self {
            MatchField::Flat(s) => {
                if s.is_empty() { Vec::new() } else { vec![s.as_str()] }
            }
            MatchField::Structured(m) => {
                let mut out = Vec::new();
                if !m.vst_basename.is_empty() { out.push(m.vst_basename.as_str()); }
                if !m.display_name.is_empty() { out.push(m.display_name.as_str()); }
                out
            }
        }
    }

    pub fn primary(&self) -> &str {
        match self {
            MatchField::Flat(s) => s,
            MatchField::Structured(m) => {
                if !m.display_name.is_empty() { &m.display_name }
                else { &m.vst_basename }
            }
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MatchRule {
    #[serde(default)]
    pub vst_basename: String,
    #[serde(default)]
    pub display_name: String,
}

// ── fallback target ────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FallbackTarget {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub acquire: Option<Acquire>,
    #[serde(default = "default_gain")]
    pub gain: f32,

    // Distribution metadata
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub download: Option<String>,
    #[serde(default)]
    pub redistributable: bool,
}

fn default_gain() -> f32 { 1.0 }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Mapping {
    #[serde(rename = "match", default)]
    pub match_: MatchField,
    #[serde(default)]
    pub fallback: FallbackTarget,
    #[serde(default)]
    pub note: String,
}

impl Mapping {
    /// A mapping with no `name` in its fallback is a "skip" marker
    /// (e.g. Patcher).  Used by the effect resolver to say
    /// `Resolution::Skipped` rather than treating it as a match.
    pub fn is_skip(&self) -> bool {
        self.fallback.name.is_empty()
    }
}

// ── acquisition ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Acquire {
    Script {
        script: String,
        provides: String,
    },
    Download {
        url: String,
        archive: String,
        extract_to: String,
        provides: String,
        #[serde(default)]
        sha256: Option<String>,
    },
}

impl Acquire {
    pub fn provides(&self) -> &str {
        match self {
            Acquire::Script { provides, .. } => provides,
            Acquire::Download { provides, .. } => provides,
        }
    }
}

// ── master chain ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MasterEntry {
    #[serde(rename = "match")]
    pub match_name: String,
    #[serde(default)]
    pub path: Option<PathBuf>,
}

// ── search paths ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SearchPaths {
    #[serde(default)]
    pub system: Vec<String>,
    #[serde(default)]
    pub user: Vec<String>,
    #[serde(default)]
    pub relative: Vec<String>,
    #[serde(default = "default_true")]
    pub include_user_config: bool,
}

fn default_true() -> bool { true }

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MissingPolicy {
    #[default]
    Warn,
    Fatal,
    Silent,
}
