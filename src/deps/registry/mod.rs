//! Plugin registry: single source of truth for what substitutes for what.
//!
//! Data flow:
//!   share/registry.json              (shipped defaults)
//!   ~/.config/circolate/registry.json (user overrides)
//!   search_paths -> PluginIndex

mod load;
mod paths;
mod schema;

use std::path::{Path, PathBuf};

pub use schema::{
    Acquire, FallbackTarget, Mapping, MatchField, MatchRule,
    MasterEntry, MissingPolicy, Registry, SearchPaths,
};
pub use paths::{install_root, user_config_dir};

use crate::deps::index::PluginIndex;

// ── resolved answer ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Resolution {
    Exact { name: String, path: PathBuf },
    Fallback { name: String, path: PathBuf, replaces: String, gain: f32 },
    FallbackMissing { name: String, replaces: String },
    NoMatch,
    Skipped { note: String },
    /// A native-effect artifact (LUT, IR, or filter coefficients) was
    /// found under `share/native_fx/`.
    NativeArtifact {
        name: String,
        path: PathBuf,
        replaces: String,
        gain: f32,
    },
    /// A native effect was named but its artifact hasn't been profiled
    /// yet.
    NativeMissing {
        name: String,
        replaces: String,
    },
}

// ── table ──────────────────────────────────────────────────────────────

pub struct RegistryTable {
    registry: Registry,
    search_paths: Vec<PathBuf>,
}

impl RegistryTable {
    pub fn load() -> anyhow::Result<Self> {
        let registry = load::load()?;
        let search_paths = paths::resolve_all(&registry.search_paths);
        Ok(RegistryTable { registry, search_paths })
    }

    pub fn on_missing(&self) -> MissingPolicy {
        self.registry.on_missing
    }

    pub fn search_paths(&self) -> &[PathBuf] {
        &self.search_paths
    }

    pub fn master_chain(&self) -> &[MasterEntry] {
        &self.registry.master
    }

    /// Iterate every mapping (instruments first, then effects).
    pub fn entries(&self) -> impl Iterator<Item = &Mapping> {
        self.registry.instruments.iter()
        .chain(self.registry.effects.iter())
    }

    // ── resolution (with a PluginIndex) ────────────────────────────────

    pub fn resolve_instrument(&self, index: &PluginIndex, name: &str) -> Resolution {
        if let Some(p) = index.find_any(name) {
            return Resolution::Exact { name: name.to_string(), path: p.clone() };
        }
        if let Some(m) = find_mapping(&self.registry.instruments, name) {
            if !m.is_skip() {
                return self.install_fallback(&m.fallback, name, index);
            }
        }
        if let Some(fb) = &self.registry.default_instrument {
            return self.install_fallback(fb, name, index);
        }
        Resolution::NoMatch
    }

    pub fn resolve_effect(&self, index: &PluginIndex, internal_name: &str) -> Resolution {
        if let Some(m) = find_mapping(&self.registry.effects, internal_name) {
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
        if let Some(fb) = &self.registry.default_effect {
            return self.install_fallback(fb, internal_name, index);
        }
        Resolution::NoMatch
    }

    // ── index-free lookups (fetch / requires / check) ──────────────────

    pub fn lookup_instrument(&self, name: &str) -> Option<&FallbackTarget> {
        if let Some(m) = find_mapping(&self.registry.instruments, name) {
            if !m.is_skip() {
                return Some(&m.fallback);
            }
        }
        self.registry.default_instrument.as_ref()
    }

    pub fn lookup_effect(&self, internal_name: &str) -> Option<&FallbackTarget> {
        if let Some(m) = find_mapping(&self.registry.effects, internal_name) {
            if !m.is_skip() {
                return Some(&m.fallback);
            }
            return None;
        }
        self.registry.default_effect.as_ref()
    }

    /// Cross-table lookup.  Returns the mapping and its fallback target.
    pub fn lookup_any(&self, name: &str) -> Option<(&Mapping, &FallbackTarget)> {
        if let Some(m) = find_mapping(&self.registry.instruments, name) {
            if !m.is_skip() {
                return Some((m, &m.fallback));
            }
        }
        if let Some(m) = find_mapping(&self.registry.effects, name) {
            if !m.is_skip() {
                return Some((m, &m.fallback));
            }
        }
        None
    }

    // ── internals ──────────────────────────────────────────────────────

    fn install_fallback(
        &self,
        fb: &FallbackTarget,
        replaces: &str,
        index: &PluginIndex,
    ) -> Resolution {
        // Native effect: artifact lives under share/native_fx/.
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

        // VST substitute: existing behaviour.
        if !fb.path.is_empty() {
            let p = PathBuf::from(&fb.path);
            if p.exists() {
                return Resolution::Fallback {
                    name: fb.name.clone(),
                    path: p,
                    replaces: replaces.to_string(),
                    gain: fb.gain,
                };
            }
        }
        if let Some(p) = index.find_any(&fb.name) {
            return Resolution::Fallback {
                name: fb.name.clone(),
                path: p.clone(),
                replaces: replaces.to_string(),
                gain: fb.gain,
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
