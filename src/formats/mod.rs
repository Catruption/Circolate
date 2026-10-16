//! Raw format parsers.  Bytes to structs and back, no policy.

pub mod flp;
pub mod flp_audio;
pub mod it;
pub mod midi;
pub mod sf2;
pub mod trackscript;

use anyhow::{anyhow, bail, Result};
use std::path::Path;

/// Detected source format from the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Flp,
    It,
    Midi,
    Trackscript,
}

impl SourceKind {
    pub fn from_path(p: &Path) -> Result<Self> {
        // Extracted trackscript archives contain a file literally called
        // `trackscript` with no extension — `ENTRY_KEY` in `archive.rs`.
        // Recognize it before the extension check so an unzipped `.tsz`
        // plays directly.
        if let Some(stem) = p.file_name().and_then(|s| s.to_str()) {
            if stem == "trackscript" {
                return Ok(SourceKind::Trackscript);
            }
        }

        let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .ok_or_else(|| anyhow!("no file extension on {}", p.display()))?;

        Ok(match ext.as_str() {
            "flp" => SourceKind::Flp,
            "it"  => SourceKind::It,
            "mid" | "midi" => SourceKind::Midi,
            "trackscript" => SourceKind::Trackscript,
            other => bail!("unknown source type .{other}"),
        })
    }
}
