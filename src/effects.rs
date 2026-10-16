//! Master-bus effect chain.
//!
//! Config in `master_fx.json` at the repo root: a list of VST3 bundles
//! applied serially to the final stereo mix, after the engine has summed
//! every voice and before the WAV writer.

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

    /// Load `master_fx.json` from the repo root.  Missing file = empty
    /// chain.  Individual load failures are warned and skipped.
    pub fn load_default(sample_rate: u32) -> Result<Self> {
        let Some(path) = find_config() else {
            return Ok(EffectChain::empty());
        };

        let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config = serde_json::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;

        let repo_root = path.parent().unwrap_or_else(|| Path::new("."));

        let mut hosts  = Vec::new();
        let mut labels = Vec::new();

        for entry in cfg.chain {
            let plugin_path = repo_root.join(&entry.path);
            if !plugin_path.exists() {
                eprintln!("  master fx: missing {}", plugin_path.display());
                continue;
            }

            let state_bytes = match &entry.state {
                Some(s) => {
                    let p = repo_root.join(s);
                    std::fs::read(&p).unwrap_or_else(|e| {
                        eprintln!("  master fx: cannot read state {}: {}", p.display(), e);
                        Vec::new()
                    })
                }
                None => Vec::new(),
            };

            let Some(path_str) = plugin_path.to_str() else {
                eprintln!("  master fx: non-UTF8 path for {}", entry.name);
                continue;
            };

            match VstHost::load(path_str, &state_bytes, sample_rate) {
                Ok(host) => {
                    println!("  master fx: loaded {}", entry.name);
                    hosts.push(host);
                    labels.push(entry.name);
                }
                Err(e) => {
                    eprintln!("  master fx: failed to load {}: {:#}", entry.name, e);
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
    let mut v = vec![PathBuf::from("master_fx.json")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("master_fx.json"));
            v.push(dir.join("../../master_fx.json"));
        }
    }
    v.into_iter().find(|p| p.exists())
}
