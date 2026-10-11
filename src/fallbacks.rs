//! Fallback plugin mappings — read from `fallbacks.json` at the repo root.
//!
//! When the converter hits a channel whose plugin isn't installed, this
//! table says what to substitute.  Lookup is by VST basename first, then
//! by display name.
//!
//! Each mapping may also carry an `acquire` block describing how to fetch
//! the fallback plugin if it's missing.  Two forms: a declarative
//! `{ url, archive, extract_to }` for the common download-and-extract
//! case, or a `{ script }` escape hatch for plugins that need real logic.

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
}

/// How to fetch a missing fallback plugin.
///
/// Tagged as `untagged` so the JSON can be either form without an
/// explicit discriminator.  Serde tries variants in declaration order;
/// putting `Script` first means a download block (which has no `script`
/// field) falls through to `Download` cleanly.
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum Acquire {
    Script {
        script: String,
        provides: String,
    },
    Download {
        url: String,
        /// One of: tar.gz, tar.xz, tar.bz2, tgz, zip, 7z
        archive: String,
        extract_to: String,
        provides: String,
        #[serde(default)]
        sha256: Option<String>,
    },
}

impl Acquire {
    /// The path that must exist after a successful fetch.  Used both as
    /// the idempotence check ("already present?") and the post-run
    /// verification ("did the script actually put something there?").
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

    /// Look for `fallbacks.json` in the usual places.  Called once at
    /// converter start, not per channel.
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
            // FLP stores Windows paths; backslashes aren't separators on
            // Linux, so Path::new would return the whole string as one
            // component.  Normalize first.
            let normalized = vst_path.replace('\\', "/");
            let base = Path::new(&normalized)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

            if let Some(&i) = self.by_basename.get(&base) {
                return Some((&self.entries[i], &self.entries[i].fallback));
            }

            // Also try stripping the extension, so matchers can be keyed
            // either as "ZENOLOGY.vst" or "ZENOLOGY".
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

    pub fn default_rule(&self) -> &DefaultRule { &self.default }

    /// All mappings, in file order.  Used by the `fetch` subcommand.
    pub fn entries(&self) -> &[Mapping] { &self.entries }
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("fallbacks.json")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("fallbacks.json"));
            v.push(dir.join("../../fallbacks.json"));
        }
    }
    v
}
