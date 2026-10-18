//! Serde types for the effect, master-chain, and search-path sections
//! of `share/fallbacks.json`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── matching ───────────────────────────────────────────────────────────

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
