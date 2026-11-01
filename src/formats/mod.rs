//! Raw format parsers.  Bytes to structs and back, no policy.

pub mod flp;
pub mod flp_audio;
pub mod it;
pub mod midi;
pub mod sf2;
pub mod halotext;

use anyhow::{anyhow, bail, Result};
use std::path::Path;

/// Detected source format from the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Flp,
    It,
    Midi,
    Vgm,
    Halotext,
}

impl SourceKind {
    pub fn from_path(p: &Path) -> Result<Self> {
        // Extracted halotext archives contain a file literally called
        // `halotext` with no extension — `ENTRY_KEY` in `archive.rs`.
        // Recognize it before the extension check so an extracted `.halo.zip`, `.hlz`, or `.tsz`
        // plays directly.
        if let Some(stem) = p.file_name().and_then(|s| s.to_str()) {
            if stem == "halotext" {
                return Ok(SourceKind::Halotext);
            }
        }

        let filename = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
        if filename.ends_with(crate::spec::SOURCE_EXT) {
            return Ok(SourceKind::Halotext);
        }
        let ext = p.extension().and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .ok_or_else(|| anyhow!("no file extension on {}", p.display()))?;

        Ok(match ext.as_str() {
            "flp" => SourceKind::Flp,
            "it"  => SourceKind::It,
            "mid" | "midi" => SourceKind::Midi,
            "vgm" | "vgz" => SourceKind::Vgm,
            "halotext" | "trackscript" => SourceKind::Halotext,
            other => bail!("unknown source type .{other}"),
        })
    }
}
