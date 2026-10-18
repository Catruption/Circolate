//! Census of plugins the corpus requires.
//!
//! Written by `census`, read by `check --scaffold-fallbacks`.
//! Aggregates every plugin name that appears in a `.flp` probe report.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const CENSUS_FILENAME: &str = "required_plugins.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequiredPlugins {
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub corpus_root: Option<String>,
    #[serde(default)]
    pub required: BTreeMap<String, Requirement>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Requirement {
    #[serde(default)]
    pub occurrences: u32,
    #[serde(default)]
    pub seen_in: Vec<String>,
}

impl RequiredPlugins {
    pub fn new(root: &Path) -> Self {
        RequiredPlugins {
            generated_at: None,
            corpus_root: Some(root.display().to_string()),
            required: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, name: &str, file: &Path) {
        if name.is_empty() {
            return;
        }
        let entry = self.required.entry(name.to_string()).or_default();
        entry.occurrences += 1;
        let path_str = file.display().to_string();
        if !entry.seen_in.contains(&path_str) {
            entry.seen_in.push(path_str);
        }
    }

    pub fn names(&self) -> Vec<&str> {
        self.required.keys().map(String::as_str).collect()
    }

    pub fn sorted_names(&self) -> Vec<(&str, u32)> {
        let mut v: Vec<(&str, u32)> = self.required
        .iter()
        .map(|(k, r)| (k.as_str(), r.occurrences))
        .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        v
    }

    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => RequiredPlugins::default(),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn add_dedupes_seen_in() {
        let mut c = RequiredPlugins::new(Path::new("."));
        c.add("Kontakt", Path::new("a.flp"));
        c.add("Kontakt", Path::new("a.flp"));
        c.add("Kontakt", Path::new("b.flp"));
        let r = c.required.get("Kontakt").unwrap();
        assert_eq!(r.occurrences, 3);
        assert_eq!(r.seen_in.len(), 2);
    }

    #[test]
    fn sorted_by_occurrence() {
        let mut c = RequiredPlugins::new(Path::new("."));
        c.add("A", Path::new("x"));
        c.add("B", Path::new("x"));
        c.add("B", Path::new("x"));
        c.add("C", Path::new("x"));
        c.add("C", Path::new("x"));
        c.add("C", Path::new("x"));
        let names: Vec<_> = c.sorted_names().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, vec!["C", "B", "A"]);
    }
}
