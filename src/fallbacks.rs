//! Fallback plugin mappings — read from `fallbacks.json` at the repo root.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct FallbackFile {
    pub version: u32,
    pub mappings: Vec<Mapping>,
    #[serde(default)]
    pub default: DefaultRule,
}

#[derive(Debug, Deserialize)]
pub struct Mapping {
    #[serde(rename = "match")]
    pub matcher: MatchRule,
    pub fallback: FallbackTarget,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct MatchRule {
    #[serde(default)]
    pub vst_basename: String,
    #[serde(default)]
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct FallbackTarget {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub acquire: Option<Acquire>,
    #[serde(default = "default_gain")]
    pub gain: f32,

    // ── new: distribution metadata ─────────────────────────────────
    /// SPDX-ish identifier or a free-form tag.  "GPL-3.0", "MIT",
    /// "proprietary-free", "commercial" are the ones the report
    /// understands; anything else is passed through verbatim.
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub download: Option<String>,
    /// True when the plugin's license permits redistribution and the
    /// binary travels cleanly (no online activation).  Only plugins
    /// marked true are eligible for `--include-plugins=free`.
    #[serde(default)]
    pub redistributable: bool,
}

fn default_gain() -> f32 { 1.0 }

#[derive(Debug, Deserialize, Clone)]
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

#[derive(Debug, Deserialize, Default)]
pub struct DefaultRule {
    #[serde(default)]
    pub fallback: Option<FallbackTarget>,
    #[serde(default = "default_on_missing")]
    pub on_missing: String,
}

fn default_on_missing() -> String { "warn".into() }

pub struct FallbackTable {
    by_basename: HashMap<String, usize>,
    by_display: HashMap<String, usize>,
    entries: Vec<Mapping>,
    default: DefaultRule,
}

impl FallbackTable {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let file: FallbackFile = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;

        let mut by_basename = HashMap::new();
        let mut by_display = HashMap::new();
        for (i, m) in file.mappings.iter().enumerate() {
            if !m.matcher.vst_basename.is_empty() {
                by_basename.insert(m.matcher.vst_basename.to_ascii_lowercase(), i);
            }
            if !m.matcher.display_name.is_empty() {
                by_display.insert(m.matcher.display_name.to_ascii_lowercase(), i);
            }
        }

        Ok(Self {
            by_basename,
            by_display,
            entries: file.mappings,
            default: file.default,
        })
    }

    pub fn load_default() -> Result<Self> {
        for p in candidate_paths() {
            if p.exists() {
                return Self::load(&p);
            }
        }
        anyhow::bail!("fallbacks.json not found in any search path")
    }

    pub fn resolve(&self, vst_path: &str, display_name: &str)
    -> Option<(&Mapping, &FallbackTarget)>
    {
        if !vst_path.is_empty() {
            let normalized = vst_path.replace('\\', "/");
            let base = Path::new(&normalized)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();

            if let Some(&i) = self.by_basename.get(&base) {
                return Some((&self.entries[i], &self.entries[i].fallback));
            }

            if let Some(stem) = base
                .strip_suffix(".vst")
                .or_else(|| base.strip_suffix(".vst3"))
                .or_else(|| base.strip_suffix(".dll"))
            {
                if let Some(&i) = self.by_basename.get(stem) {
                    return Some((&self.entries[i], &self.entries[i].fallback));
                }
            }
        }
        if !display_name.is_empty() {
            let d = display_name.to_ascii_lowercase();
            if let Some(&i) = self.by_display.get(&d) {
                return Some((&self.entries[i], &self.entries[i].fallback));
            }
        }
        None
    }

    /// Match against any of the three identifying fields: matcher
    /// basename (with or without extension), matcher display name,
    /// or fallback target name.  Used by the requires builder, where
    /// we know a plugin's display name but not its path.
    pub fn lookup_any(&self, name: &str) -> Option<(&Mapping, &FallbackTarget)> {
        let lower = name.to_ascii_lowercase();
        for m in &self.entries {
            let disp = m.matcher.display_name.to_ascii_lowercase();
            let base = m.matcher.vst_basename.to_ascii_lowercase();
            let base_stem = base
                .strip_suffix(".vst")
                .or_else(|| base.strip_suffix(".vst3"))
                .or_else(|| base.strip_suffix(".dll"))
                .unwrap_or(&base);
            let fb = m.fallback.name.to_ascii_lowercase();
            if disp == lower || base == lower || base_stem == lower || fb == lower {
                return Some((m, &m.fallback));
            }
        }
        None
    }

    pub fn default_rule(&self) -> &DefaultRule { &self.default }

    pub fn entries(&self) -> &[Mapping] { &self.entries }
}

/// Repo root, discovered by walking up from the current executable
/// looking for a directory containing both `Cargo.toml` and
/// `fallbacks.json`.  Falls back to the current working directory when
/// running from a source checkout.
/// Repo root, discovered by walking up from the current executable
/// looking for a directory containing `Cargo.toml`.  The presence of
/// `Cargo.toml` alone identifies the root; `fallbacks.json` may live at
/// the root or under `share/` depending on how the tree is laid out.
/// Repo root, discovered by walking up from the current executable
/// looking for a directory containing `Cargo.toml`.
pub fn repo_root() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        let mut d = exe.parent().map(Path::to_path_buf);
        while let Some(p) = d {
            if p.join("Cargo.toml").is_file() {
                return Some(p);
            }
            d = p.parent().map(Path::to_path_buf);
        }
    }
    if Path::new("Cargo.toml").is_file() {
        return std::env::current_dir().ok();
    }
    None
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("fallbacks.json")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("fallbacks.json"));
            v.push(dir.join("../../fallbacks.json"));
        }
    }
    if let Some(root) = repo_root() {
        v.push(root.join("fallbacks.json"));
        v.push(root.join("share/fallbacks.json"));
    }
    if Path::new("share/fallbacks.json").is_file() {
        v.push(PathBuf::from("share/fallbacks.json"));
    }
    v
}