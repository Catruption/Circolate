//! Plugin requirements manifest — `requires.json` inside `.tsz`
//! archives.
//!
//! Records every plugin a project uses that cannot or should not be
//! embedded: proprietary binaries, online-activated plugins, anything
//! whose license terms rule out redistribution.  Also records the
//! fallback substitutions the converter applied, so a receiver knows
//! what the archive will actually sound like.
//!
//! This is a pointer file.  It never downloads anything; it tells the
//! user where to get a plugin and lets them decide.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::convert::AuxFile;
use crate::deps::registry::RegistryTable;

pub const FILENAME: &str = "requires.json";
pub const FORMAT_TAG: &str = "trackscript-requires";
pub const FORMAT_VERSION: u32 = 1;

// ── data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Requires {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub plugins: Vec<PluginNeed>,
    #[serde(default)]
    pub fallbacks: Vec<FallbackUse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginNeed {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub download: Option<String>,
    #[serde(default)]
    pub redistributable: bool,
    #[serde(default)]
    pub used_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackUse {
    pub missing: String,
    pub installed_fallback: String,
    #[serde(default)]
    pub note: String,
}

// ── bundle discovery ───────────────────────────────────────────────────

/// A plugin bundle on disk that a project references.  Used by both the
/// requires builder (for the metadata) and the archive writer (for
/// embedding).
#[derive(Debug, Clone)]
pub struct Bundle {
    pub name: String,
    pub license: String,
    pub homepage: Option<String>,
    pub download: Option<String>,
    pub redistributable: bool,
    /// Path to the `.vst3` bundle on disk, if it still exists.  None
    /// means the plugin was missing at convert time.
    pub path: Option<PathBuf>,
}

/// Extract every VST3 bundle referenced by the conversion output.
/// Reads the `.instrument` sidecars; each carries the plugin's display
/// name and its path on the machine that made the file.
pub fn bundles_from_aux(aux: &[AuxFile]) -> Vec<Bundle> {
    let table = RegistryTable::load().ok();
    let mut out: Vec<Bundle> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for file in aux {
        if !file.rel_path.ends_with(".instrument") {
            continue;
        }
        let text = String::from_utf8_lossy(&file.bytes);
        let Ok(inst) = crate::deps::instruments::parse(&text, &file.rel_path) else {
            continue;
        };
        if !inst.is_vst3() {
            continue;
        }

        // The display name is what the fallback table keys on.  Fall
        // back to the internal name if the sidecar omitted it.
        let name = if !inst.plugin.is_empty() {
            inst.plugin.clone()
        } else if !inst.name.is_empty() {
            inst.name.clone()
        } else {
            continue;
        };

        if !seen.insert(name.to_ascii_lowercase()) {
            continue;
        }

        let (license, homepage, download, redistributable) = table
            .as_ref()
            .and_then(|t| t.lookup_any(&name))
            .map(|(_m, tgt)| {
                (
                    tgt.license.clone(),
                    tgt.homepage.clone(),
                    tgt.download.clone(),
                    tgt.redistributable,
                )
            })
            .unwrap_or_default();

        let path = if inst.vst_path.is_empty() {
            None
        } else {
            let p = PathBuf::from(&inst.vst_path);
            if p.exists() { Some(p) } else { None }
        };

        out.push(Bundle {
            name,
            license,
            homepage,
            download,
            redistributable,
            path,
        });
    }

    out
}

// ── build ──────────────────────────────────────────────────────────────

pub fn build_from_aux(aux: &[AuxFile]) -> Requires {
    let bundles = bundles_from_aux(aux);
    let table = RegistryTable::load().ok();

    let mut by_name: BTreeMap<String, PluginNeed> = BTreeMap::new();
    let mut fallbacks: Vec<FallbackUse> = Vec::new();
    let mut seen_fb: HashSet<(String, String)> = HashSet::new();

    // Sidecar scan, this time for the fallback substitutions.
    for file in aux {
        if !file.rel_path.ends_with(".instrument") {
            continue;
        }
        let text = String::from_utf8_lossy(&file.bytes);
        let Ok(inst) = crate::deps::instruments::parse(&text, &file.rel_path) else {
            continue;
        };
        if !inst.is_vst3() || !inst.fallback || inst.replaces.is_empty() {
            continue;
        }
        let key = (inst.replaces.clone(), inst.plugin.clone());
        if seen_fb.insert(key.clone()) {
            let note = table
                .as_ref()
                .and_then(|t| t.lookup_any(&inst.replaces))
                .map(|(m, _)| m.note.clone())
                .unwrap_or_default();
            fallbacks.push(FallbackUse {
                missing: key.0,
                installed_fallback: key.1,
                note,
            });
        }
    }

    // Now the plugin list.  Redistributable plugins are eligible for
    // embedding and never appear in `requires`.
    for b in bundles {
        if b.redistributable {
            continue;
        }
        let entry = by_name.entry(b.name.clone()).or_insert_with(|| PluginNeed {
            name: b.name.clone(),
            kind: "vst3".into(),
            license: b.license.clone(),
            homepage: b.homepage.clone(),
            download: b.download.clone(),
            redistributable: false,
            used_by: Vec::new(),
        });
        if !entry.used_by.contains(&b.name) {
            entry.used_by.push(b.name.clone());
        }
    }

    Requires {
        format: FORMAT_TAG.into(),
        version: FORMAT_VERSION,
        plugins: by_name.into_values().collect(),
        fallbacks,
    }
}

// ── report ─────────────────────────────────────────────────────────────

/// Human-readable summary, meant to be printed once.  Lines are plain
/// text so the caller can decide where to route them.
pub fn report_lines(req: &Requires) -> Vec<String> {
    let mut out = Vec::new();
    if req.plugins.is_empty() && req.fallbacks.is_empty() {
        return out;
    }

    if !req.plugins.is_empty() {
        out.push("This project references plugins you don't have:".into());
        for p in &req.plugins {
            let tag = match p.license.as_str() {
                "commercial" => " [commercial]",
                "proprietary-free" => " [free]",
                "GPL-3.0" | "GPL" | "MIT" => " [free / open]",
                _ => "",
            };
            out.push(format!("  {}{}", p.name, tag));
            if let Some(url) = &p.homepage {
                out.push(format!("    {}", url));
            }
            if let Some(url) = &p.download {
                out.push(format!("    direct: {}", url));
            }
        }
    }

    if !req.fallbacks.is_empty() {
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push("The archive falls back to:".into());
        for f in &req.fallbacks {
            if f.note.is_empty() {
                out.push(format!("  {} → {}", f.missing, f.installed_fallback));
            } else {
                out.push(format!(
                    "  {} → {} ({})",
                    f.missing, f.installed_fallback, f.note
                ));
            }
        }
    }

    out
}

pub fn report(req: &Requires) {
    for line in report_lines(req) {
        println!("{}", line);
    }
}

// ── io ─────────────────────────────────────────────────────────────────

pub fn parse(text: &str) -> Result<Requires> {
    let r: Requires = serde_json::from_str(text).context("parsing requires.json")?;
    if r.format != FORMAT_TAG {
        anyhow::bail!("unexpected requires format: {}", r.format);
    }
    Ok(r)
}

pub fn load(path: &Path) -> Result<Requires> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    parse(&text)
}