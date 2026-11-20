//! Master-bus effect chain.
//!
//! Config in `master_fx.json`: a list of VST3 bundles applied serially
//! to the final stereo mix, after the engine has summed every voice and
//! before the WAV writer.
//!
//! Config lookup order:
//!   1. `$XDG_CONFIG_HOME/circolate/master_fx.json`  (user override)
//!   2. `/usr/share/circolate/master_fx.json`         (shipped default)
//!
//! `path:` is a canonical plugin name or a portable relative path.  It
//! is resolved through the shared plugin index (the Circolate system
//! directories) first, then against the config file's directory,
//! `system_data()`, and `user_data()`.  `state:` is resolved relative
//! to the config.  Machine-specific absolute paths are never used.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::hosting::vst::ffi::VstHost;

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default)]
    chain: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    name: String,
    path: String,
    #[serde(default)]
    state: Option<String>,
}

pub struct EffectChain {
    hosts:  Vec<VstHost>,
    labels: Vec<String>,
}

impl EffectChain {
    pub fn empty() -> Self {
        EffectChain { hosts: Vec::new(), labels: Vec::new() }
    }

    /// Load `master_fx.json`.  Missing file = empty chain.  Individual
    /// load failures are reported through the log bus and skipped.
    pub fn load_default(sample_rate: u32) -> Result<Self> {
        let Some(path) = find_config() else {
            return Ok(EffectChain::empty());
        };

        let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config = serde_json::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;

        let config_dir = path.parent().unwrap_or_else(|| Path::new("."));

        let mut hosts  = Vec::new();
        let mut labels = Vec::new();

        for entry in cfg.chain {
            let Some(plugin_path) = resolve_plugin_ref(&entry.path, config_dir) else {
                crate::log_event!("player.master_fx.missing", path: entry.path.clone());
                continue;
            };

            let state_bytes = match &entry.state {
                Some(s) => match resolve_relative(s, config_dir) {
                    Some(p) => std::fs::read(&p).unwrap_or_else(|e| {
                        crate::log_event!(
                            "player.master_fx.state_read",
                            path: p.display(),
                                          err: e,
                        );
                        Vec::new()
                    }),
                    None => {
                        crate::log_event!("player.master_fx.state_file_missing", path: s.clone());
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };

            let Some(path_str) = plugin_path.to_str() else {
                crate::log_event!("player.master_fx.non_utf8", name: entry.name);
                continue;
            };

            match VstHost::load(path_str, &state_bytes, sample_rate) {
                Ok(host) => {
                    crate::log_event!("player.master_fx.loaded", name: entry.name.clone());
                    hosts.push(host);
                    labels.push(entry.name);
                }
                Err(e) => {
                    crate::log_event!(
                        "player.master_fx.load_failed",
                        name: entry.name,
                        err: format!("{:#}", e),
                    );
                }
            }
        }

        Ok(EffectChain { hosts, labels })
    }

    pub fn is_empty(&self) -> bool { self.hosts.is_empty() }
    pub fn len(&self) -> usize    { self.hosts.len() }

    pub fn describe(&self) -> String {
        if self.hosts.is_empty() {
            "master fx: none".into()
        } else {
            format!("master fx: {}", self.labels.join(" -> "))
        }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for host in &mut self.hosts {
            host.process_inplace(l, r);
        }
    }
}

fn find_config() -> Option<PathBuf> {
    let user = crate::paths::user_config().join("master_fx.json");
    if user.is_file() {
        return Some(user);
    }
    let legacy = crate::paths::legacy_user_config().join("master_fx.json");
    if legacy.is_file() {
        return Some(legacy);
    }
    let system = crate::paths::system_data().join("master_fx.json");
    if system.is_file() {
        return Some(system);
    }
    None
}

/// Resolve a VST named in the master-FX config.  The shared plugin
/// index is checked first — it is built from the same Circolate system
/// directories used for every other file.  A config entry may be a
/// canonical name or a portable relative path; an absolute path is
/// never used.
fn resolve_plugin_ref(raw: &str, config_dir: &Path) -> Option<PathBuf> {
    if let Some(p) = crate::deps::plugins::PluginTable::get().resolve_reference(raw) {
        return Some(p);
    }
    if Path::new(raw).is_absolute() {
        return None;
    }
    resolve_relative(raw, config_dir)
}

/// Resolve a possibly-relative path from the config.  Relative paths
/// are tried against (in order) the config file's parent directory,
/// `system_data()`, and `user_data()`, returning the first candidate
/// that exists.
fn resolve_relative(rel: &str, config_dir: &Path) -> Option<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return None;
    }
    [
        config_dir.join(rel),
        crate::paths::system_data().join(rel),
        crate::paths::user_data().join(rel),
    ]
    .into_iter()
    .find(|p| p.exists())
}
