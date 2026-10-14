//! Storage abstraction: everything a project load needs, backed by
//! either a directory tree or a `.tsz` archive.

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
    /// by extension.  Paths use '/' separators, relative to the source
    /// root.
    fn list(&self, prefix: &str, ext: Option<&str>) -> Vec<String>;

    /// Recursive listing under `prefix`.  Default returns direct
    /// children; directory sources and archives override.
    fn list_recursive(&self, prefix: &str) -> Vec<String> {
        self.list(prefix, None)
    }

    /// Stable identity for caching extracted content.  `None` means the
    /// source has no natural key (dirs, mainly).
    fn cache_key(&self) -> Option<String> {
        None
    }

    fn describe(&self) -> String;
}

// ── directory-backed source ────────────────────────────────────────────

pub struct DirSource {
    base: PathBuf,
    trackscript_name: String,
}

impl DirSource {
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

    fn list_recursive(&self, prefix: &str) -> Vec<String> {
        let start = if prefix.is_empty() {
            self.base.clone()
        } else {
            self.base.join(prefix)
        };
        let mut out = Vec::new();
        walk_files(&start, &self.base, &mut out);
        out.sort();
        out
    }

    fn describe(&self) -> String {
        self.base.display().to_string()
    }
}

fn walk_files(dir: &Path, base: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
        if md.file_type().is_dir() {
            walk_files(&p, base, out);
        } else if md.file_type().is_file() {
            if let Ok(rel) = p.strip_prefix(base) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}