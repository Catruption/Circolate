//! Plugin loudness inventory.
//!
//! Written by `measure --all`, read by `RegistryTable::resolve_instrument`
//! to compute fallback gains from measured levels instead of hand-tuned
//! `"gain"` fields. Auto-regenerated when plugins change; never edited
//! by hand.
//!
//! ## Shape
//!
//! ```json
//! {
//!   "generated_at": "epoch:1760799780",
//!   "plugins": {
//!     "Surge XT": {
//!       "kind": "vst3",
//!       "path": "plugins/surge-xt-linux-1.3.4-pluginsonly/Surge XT.vst3",
//!       "rms_db": -8.3,
//!       "peak_db": -2.1
//!     },
//!     "Decent Sampler": {
//!       "kind": "vst3",
//!       "path": "...",
//!       "note": "silent at default — likely needs a preset"
//!     }
//!   }
//! }
//! ```
//!
//! A missing `rms_db` (or `null`) means the plugin produced silence at
//! default. That is diagnostic information, not an error: those plugins
//! need preset loading before they can serve as fallbacks.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const INVENTORY_FILENAME: &str = "plugins_inventory.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginInventory {
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginLevel {
    pub kind: String,
    pub path: String,
    #[serde(default)]
    pub rms_db: Option<f32>,
    #[serde(default)]
    pub peak_db: Option<f32>,
    #[serde(default)]
    pub note: Option<String>,
}

impl PluginInventory {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(inv) => inv,
                Err(e) => {
                    eprintln!(
                        "plugins_inventory: {} failed to parse ({e:#}); treating as empty",
                              path.display()
                    );
                    PluginInventory::default()
                }
            },
            Err(_) => PluginInventory::default(),
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

    pub fn finalize(&mut self) {
        if self.generated_at.is_none() {
            use std::time::{SystemTime, UNIX_EPOCH};
            if let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) {
                self.generated_at = Some(format!("epoch:{}", d.as_secs()));
            }
        }
    }

    /// Exact match, then normalized (same rules as
    /// `deps::index::normalize_plugin_key`, so `"DecentSampler"` finds
    /// a stored `"Decent Sampler"` entry).
    pub fn level_for(&self, name: &str) -> Option<&PluginLevel> {
        if let Some(l) = self.plugins.get(name) {
            return Some(l);
        }
        let want = normalize(name);
        self.plugins
        .iter()
        .find(|(k, _)| normalize(k) == want)
        .map(|(_, v)| v)
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

fn normalize(s: &str) -> String {
    s.chars()
    .filter(|c| c.is_ascii_alphanumeric())
    .map(|c| c.to_ascii_lowercase())
    .collect()
}

impl PluginLevel {
    pub fn from_measurement(
        kind: impl Into<String>,
        path: impl Into<String>,
        rms_db: f32,
        peak_db: f32,
    ) -> Self {
        // Only flag as silent if *both* are non-finite. A percussive
        // patch can have finite peak with near-silent RMS in the
        // measured steady window; that is still a usable level,
        // measured by peak.
        let both_silent = !rms_db.is_finite() && !peak_db.is_finite();
        PluginLevel {
            kind: kind.into(),
            path: path.into(),
            rms_db: if rms_db.is_finite() { Some(rms_db) } else { None },
            peak_db: if peak_db.is_finite() { Some(peak_db) } else { None },
            note: if both_silent {
                Some("silent at default — likely needs a preset".into())
            } else {
                None
            },
        }
    }
}
