// src/deps/fallbacks.rs
//! Ordered fallback candidates, plus effects chains, master chain, and
//! search paths.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::deps::index::PluginIndex;
use crate::deps::inventory::PluginInventory;
use crate::deps::registry::schema::{Mapping, MasterEntry, MissingPolicy, SearchPaths};

pub const FALLBACKS_FILENAME: &str = "fallbacks.json";
pub const DEFAULT_RULE_KEY: &str = "__default__";
pub const DEFAULT_CATEGORY: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbacksTable {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub categories: BTreeMap<String, CategoryTarget>,
    #[serde(default)]
    pub instruments: BTreeMap<String, Vec<FallbackCandidate>>,
    #[serde(default)]
    pub effects: Vec<Mapping>,
    #[serde(default)]
    pub master: Vec<MasterEntry>,
    #[serde(default)]
    pub search_paths: SearchPaths,
    #[serde(default)]
    pub on_missing: MissingPolicy,
}

fn one() -> u32 { 1 }

impl Default for FallbacksTable {
    fn default() -> Self {
        let mut categories = BTreeMap::new();
        categories.insert("drums".into(),    CategoryTarget { rms_db: -16.0, peak_db: -4.0 });
        categories.insert("bass".into(),     CategoryTarget { rms_db: -20.0, peak_db: -6.0 });
        categories.insert("leads".into(),    CategoryTarget { rms_db: -18.0, peak_db: -6.0 });
        categories.insert("pads".into(),     CategoryTarget { rms_db: -22.0, peak_db: -10.0 });
        categories.insert("keys".into(),     CategoryTarget { rms_db: -20.0, peak_db: -8.0 });
        categories.insert("chiptune".into(), CategoryTarget { rms_db: -16.0, peak_db: -4.0 });
        categories.insert("unknown".into(),  CategoryTarget { rms_db: -24.0, peak_db: -10.0 });
        categories.insert("default".into(),  CategoryTarget { rms_db: -18.0, peak_db: -6.0 });
        FallbacksTable {
            version: 1,
            categories,
            instruments: BTreeMap::new(),
            effects: Vec::new(),
            master: Vec::new(),
            search_paths: SearchPaths::default(),
            on_missing: MissingPolicy::Warn,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CategoryTarget {
    pub rms_db: f32,
    pub peak_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackCandidate {
    pub name: String,
    #[serde(default = "vst3_kind")]
    pub kind: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

fn vst3_kind() -> String { "vst3".into() }

#[derive(Debug, Clone)]
pub struct PickedFallback {
    pub name: String,
    pub path: PathBuf,
    pub category: String,
    pub computed_gain: f32,
    pub measured_rms_db: Option<f32>,
    pub measured_peak_db: Option<f32>,
    pub via_default: bool,
    pub state_path: Option<PathBuf>,
}

impl FallbacksTable {
    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str(&text) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!(
                    "fallbacks: {} failed to parse ({e:#})",
                          path.display()
                );
                None
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)
        .with_context(|| format!("writing {}", path.display()))
    }

    pub fn pick(
        &self,
        name: &str,
        index: &PluginIndex,
        inventory: &PluginInventory,
    ) -> Option<PickedFallback> {
        let specific = self
        .instruments
        .get(name)
        .filter(|chain| !chain.is_empty());

        if let Some(chain) = specific {
            if let Some(picked) = self.try_chain(chain, index, inventory, false) {
                return Some(picked);
            }
        }

        // Normalized lookup: strip " #N" suffix, lowercase, drop non-alnum.
        // Fixes ZENOLOGY #4 matching ZENOLOGY, Serum #3 matching Serum,
        // 3x Osc #2 matching 3x Osc.
        let want = normalize_key(name);
        if !want.is_empty() {
            for (k, chain) in &self.instruments {
                if normalize_key(k) == want && !chain.is_empty() {
                    if let Some(picked) = self.try_chain(chain, index, inventory, false) {
                        return Some(picked);
                    }
                }
            }
        }

        let default_chain = self.instruments.get(DEFAULT_RULE_KEY)?;
        self.try_chain(default_chain, index, inventory, true)
    }

    fn try_chain(
        &self,
        candidates: &[FallbackCandidate],
        index: &PluginIndex,
        inventory: &PluginInventory,
        via_default: bool,
    ) -> Option<PickedFallback> {
        for cand in candidates {
            let Some(path) = index.find_any(&cand.name).cloned() else {
                continue;
            };
            let category = cand.category.as_deref().unwrap_or(DEFAULT_CATEGORY);
            let target = self
            .categories
            .get(category)
            .or_else(|| self.categories.get(DEFAULT_CATEGORY))
            .copied()
            .unwrap_or(CategoryTarget { rms_db: -18.0, peak_db: -6.0 });

            let level = inventory.level_for(&cand.name);
            let measured_rms = level.and_then(|l| l.rms_db);
            let measured_peak = level.and_then(|l| l.peak_db);

            let gain = match (measured_rms, measured_peak) {
                (Some(rms), _) => compute_gain(rms, target.rms_db),
                (None, Some(peak)) => compute_gain(peak, target.peak_db),
                (None, None) => 1.0,
            };

            return Some(PickedFallback {
                name: cand.name.clone(),
                        path,
                        category: category.to_string(),
                        computed_gain: gain,
                        measured_rms_db: measured_rms,
                        measured_peak_db: measured_peak,
                        via_default,
                        state_path: cand.state.as_ref().map(PathBuf::from),
            });
        }
        None
    }

    pub fn scaffold_missing(&mut self, required: &[&str]) {
        for name in required {
            self.instruments.entry((*name).to_string()).or_default();
        }
    }

    pub fn is_empty_rule(&self, name: &str) -> bool {
        self.instruments.get(name).map_or(false, |v| v.is_empty())
    }
}

fn compute_gain(measured_db: f32, target_db: f32) -> f32 {
    if !measured_db.is_finite() || !target_db.is_finite() {
        return 1.0;
    }
    let delta = (target_db - measured_db).clamp(-24.0, 24.0);
    10f32.powf(delta / 20.0)
}

fn normalize_key(s: &str) -> String {
    let base = if let Some(idx) = s.rfind(" #") {
        let tail = &s[idx + 2..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            &s[..idx]
        } else {
            s
        }
    } else {
        s
    };
    base.chars()
    .filter(|c| c.is_ascii_alphanumeric())
    .map(|c| c.to_ascii_lowercase())
    .collect()
}
