//! Storage abstraction: everything a project load needs, backed by
//! either a directory tree or a `.tsz` archive.
//!
//! The well-known key `"trackscript"` always resolves to the source's
//! primary trackscript, regardless of the underlying filename.  Every
//! other path is treated literally relative to the source root.

use anyhow::Result;
use std::path::{Path, PathBuf};

pub const ENTRY_KEY: &str = "trackscript";

pub trait AssetSource: Send + Sync {
    fn read_bytes(&self, rel: &str) -> Result<Option<Vec<u8>>>;

    fn read_text(&self, rel: &str) -> Result<Option<String>> {
        match self.read_bytes(rel)? {
            Some(b) => Ok(Some(String::from_utf8(b)?)),
            None => Ok(None),
        }
    }

    /// Direct children of `prefix` (no recursion), optionally filtered
    /// by extension.  Returned paths use '/' separators and are
    /// relative to the source root.
    fn list(&self, prefix: &str, ext: Option<&str>) -> Vec<String>;

    fn describe(&self) -> String;
}

// ── directory-backed source ────────────────────────────────────────────

pub struct DirSource {
    base: PathBuf,
    trackscript_name: String,
}

impl DirSource {
    /// `base` is the directory that holds `trackscript`, `samples/`,
    /// `instruments/`, and `samples.json`.  `trackscript_name` is the
    /// actual filename of the trackscript inside `base`.
    pub fn new(base: impl Into<PathBuf>, trackscript_name: impl Into<String>) -> Self {
        DirSource {
            base: base.into(),
            trackscript_name: trackscript_name.into(),
        }
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    fn resolve(&self, rel: &str) -> PathBuf {
        if rel == ENTRY_KEY {
            self.base.join(&self.trackscript_name)
        } else {
            self.base.join(rel)
        }
    }
}

impl AssetSource for DirSource {
    fn read_bytes(&self, rel: &str) -> Result<Option<Vec<u8>>> {
        let p = self.resolve(rel);
        match std::fs::read(&p) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn list(&self, prefix: &str, ext: Option<&str>) -> Vec<String> {
        let dir = if prefix.is_empty() {
            self.base.clone()
        } else {
            self.base.join(prefix)
        };
        let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
        let mut out: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if !p.is_file() {
                    return None;
                }
                if let Some(want) = ext {
                    if p.extension().and_then(|s| s.to_str()) != Some(want) {
                        return None;
                    }
                }
                let name = p.file_name()?.to_str()?;
                Some(if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{}/{}", prefix, name)
                })
            })
            .collect();
        out.sort();
        out
    }

    fn describe(&self) -> String {
        self.base.display().to_string()
    }
}