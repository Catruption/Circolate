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

    pub fn resolve(&self, vst_path: &str, display_name: &str)
    -> Option<(&Mapping, &FallbackTarget)>
    {
        if !vst_path.is_empty() {
            let base = Path::new(vst_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
            if let Some(&i) = self.by_basename.get(&base) {
                return Some((&self.entries[i], &self.entries[i].fallback));
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
}
