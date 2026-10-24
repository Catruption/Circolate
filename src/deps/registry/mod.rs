//! Resolution facade.

pub mod paths;
pub mod schema;

use std::path::PathBuf;

pub use schema::{
    Acquire, FallbackTarget, Mapping, MasterEntry, MissingPolicy,
};
pub use paths::native_fx_dir;

use crate::deps::fallbacks::{FallbacksTable, FALLBACKS_FILENAME};
use crate::deps::index::PluginIndex;
use crate::deps::inventory::{PluginInventory, INVENTORY_FILENAME};

#[derive(Debug, Clone)]
pub enum Resolution {
    Exact { name: String, path: PathBuf },
    Fallback {
        name: String,
        path: PathBuf,
        replaces: String,
        gain: f32,
        via_default: bool,
        state: Option<PathBuf>,
    },
    FallbackMissing { name: String, replaces: String },
    NoMatch,
    Skipped { note: String },
    NativeArtifact {
        name: String,
        path: PathBuf,
        replaces: String,
        gain: f32,
    },
    NativeMissing {
        name: String,
        replaces: String,
    },
}

pub struct RegistryTable {
    fallbacks: FallbacksTable,
    inventory: Option<PluginInventory>,
    search_paths: Vec<PathBuf>,
}

impl RegistryTable {
    pub fn load() -> anyhow::Result<Self> {
        // The fallbacks table is loaded from the system data directory
        // (e.g., /usr/share/circolate/fallbacks.json).
        // If we need to support user overrides for fallbacks specifically,
        // they would be checked in crate::paths::user_config() first.
        let share = crate::paths::system_data();
        let fb_path = share.join(FALLBACKS_FILENAME);

        let fallbacks = FallbacksTable::load(&fb_path)
        .ok_or_else(|| anyhow::anyhow!(
            "{} missing or malformed", fb_path.display()
        ))?;

        let mut inv_path = crate::paths::user_cache().join(INVENTORY_FILENAME);
        if !inv_path.is_file() {
            inv_path = share.join(INVENTORY_FILENAME);
        }
        let inv = PluginInventory::load(&inv_path);
        let inventory = if inv.is_empty() { None } else { Some(inv) };

        let search_paths = paths::resolve_all(&fallbacks.search_paths);

        Ok(RegistryTable { fallbacks, inventory, search_paths })
    }

    pub fn on_missing(&self) -> MissingPolicy {
        self.fallbacks.on_missing
    }

    pub fn search_paths(&self) -> &[PathBuf] {
        &self.search_paths
    }

    pub fn master_chain(&self) -> &[MasterEntry] {
        &self.fallbacks.master
    }

    pub fn entries(&self) -> impl Iterator<Item = &Mapping> {
        self.fallbacks.effects.iter()
    }

    pub fn resolve_instrument(&self, index: &PluginIndex, name: &str) -> Resolution {
        if let Some(p) = index.find_any(name) {
            return Resolution::Exact { name: name.to_string(), path: p.clone() };
        }
        if let Some(inv) = &self.inventory {
            if let Some(picked) = self.fallbacks.pick(name, index, inv) {
                return Resolution::Fallback {
                    name: picked.name,
                    path: picked.path,
                    replaces: name.to_string(),
                    gain: picked.computed_gain,
                    via_default: picked.via_default,
                    state: picked.state_path,
                };
            }
        }
        Resolution::NoMatch
    }

    pub fn resolve_effect(&self, index: &PluginIndex, internal_name: &str) -> Resolution {
        if let Some(m) = find_mapping(&self.fallbacks.effects, internal_name) {
            if m.is_skip() {
                return Resolution::Skipped {
                    note: if m.note.is_empty() {
                        "no substitute configured".to_string()
                    } else {
                        m.note.clone()
                    },
                };
            }
            return self.install_fallback(&m.fallback, internal_name, index);
        }
        Resolution::NoMatch
    }

    pub fn lookup_effect(&self, internal_name: &str) -> Option<&FallbackTarget> {
        if let Some(m) = find_mapping(&self.fallbacks.effects, internal_name) {
            if !m.is_skip() {
                return Some(&m.fallback);
            }
            return None;
        }
        None
    }

    pub fn lookup_any(&self, name: &str) -> Option<(&Mapping, &FallbackTarget)> {
        if let Some(m) = find_mapping(&self.fallbacks.effects, name) {
            if !m.is_skip() {
                return Some((m, &m.fallback));
            }
        }
        None
    }

    fn install_fallback(
        &self,
        fb: &FallbackTarget,
        replaces: &str,
        index: &PluginIndex,
    ) -> Resolution {
        if fb.kind == "native" {
            let Some(dir) = paths::native_fx_dir() else {
                return Resolution::NativeMissing {
                    name: fb.name.clone(),
                    replaces: replaces.to_string(),
                };
            };
            let lut = dir.join(format!("{}.lut.bin", fb.name));
            return if lut.exists() {
                Resolution::NativeArtifact {
                    name: fb.name.clone(),
                    path: lut,
                    replaces: replaces.to_string(),
                    gain: fb.gain,
                }
            } else {
                Resolution::NativeMissing {
                    name: fb.name.clone(),
                    replaces: replaces.to_string(),
                }
            };
        }

        if !fb.path.is_empty() {
            let p = PathBuf::from(&fb.path);
            if p.exists() {
                return Resolution::Fallback {
                    name: fb.name.clone(),
                    path: p,
                    replaces: replaces.to_string(),
                    gain: fb.gain,
                    via_default: false,
                    state: None,
                };
            }
        }
        if let Some(p) = index.find_any(&fb.name) {
            return Resolution::Fallback {
                name: fb.name.clone(),
                path: p.clone(),
                replaces: replaces.to_string(),
                gain: fb.gain,
                via_default: false,
                state: None,
            };
        }
        Resolution::FallbackMissing {
            name: fb.name.clone(),
            replaces: replaces.to_string(),
        }
    }
}

fn find_mapping<'a>(table: &'a [Mapping], name: &str) -> Option<&'a Mapping> {
    let key = name.to_ascii_lowercase();
    table.iter().find(|m| {
        m.match_.candidates().iter().any(|c| c.to_ascii_lowercase() == key)
    })
}
