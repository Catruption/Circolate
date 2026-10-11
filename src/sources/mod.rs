// src/sources/mod.rs
pub mod flp;
pub mod it;
pub mod trackscript;
pub mod flp_audio;

use anyhow::{anyhow, bail, Result};
use std::path::Path;

/// Detected source format from the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Flp,
    It,
    Trackscript,
}

impl SourceKind {
    pub fn from_path(p: &Path) -> Result<Self> {
        let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .ok_or_else(|| anyhow!("no file extension on {}", p.display()))?;
        Ok(match ext.as_str() {
            "flp" => SourceKind::Flp,
            "it"  => SourceKind::It,
            "trackscript" => SourceKind::Trackscript,
            other => bail!("unknown source type .{other}"),
        })
    }
}
