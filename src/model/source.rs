//! Which format a file is.

use anyhow::{anyhow, Result};
use std::path::Path;

use crate::spec::{LEGACY_SOURCE_EXT, OLD_SOURCE_EXT, SOURCE_EXT};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Flp,
    It,
    Midi,
    Vgm,
    Halotext,
}

impl SourceKind {
    /// Detect the source format from a file path.
    ///
    /// Halotext sources have four spellings, all recognized here:
    ///
    ///   * `.halo.txt` — the current extension. This is a *compound*
    ///     extension: `Path::extension()` returns `"txt"`, so the
    ///     check has to look at the full path suffix, not the last
    ///     dot-separated component.
    ///   * `.halotext` — legacy extension from earlier versions.
    ///   * `.trackscript` — the original extension, kept for
    ///     backwards compatibility.
    ///   * a file literally named `halotext` with no extension —
    ///     `ENTRY_KEY` in `archive.rs`, produced when a `.halo.zip`,
    ///     `.hlz`, or `.tsz` archive is extracted. Recognized before
    ///     the suffix checks so an extracted archive round-trips.
    pub fn from_path(p: &Path) -> Result<Self> {
        // Extracted archive: bare filename, no extension.
        if p.file_name().and_then(|n| n.to_str()) == Some("halotext") {
            return Ok(SourceKind::Halotext);
        }

        // Compound `.halo.txt` and single-dot `.halotext` /
        // `.trackscript` all need the full-path suffix check:
        // `Path::extension()` on `song.halo.txt` returns `Some("txt")`,
        // not `Some("halo")`.
        let path_lower = p.to_string_lossy().to_ascii_lowercase();
        if path_lower.ends_with(SOURCE_EXT)
            || path_lower.ends_with(LEGACY_SOURCE_EXT)
            || path_lower.ends_with(OLD_SOURCE_EXT)
            {
                return Ok(SourceKind::Halotext);
            }

            let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .ok_or_else(|| anyhow!("no extension on {}", p.display()))?;
            match ext.as_str() {
                "flp" => Ok(SourceKind::Flp),
                "it" => Ok(SourceKind::It),
                "mid" | "midi" => Ok(SourceKind::Midi),
                "vgm" | "vgz" => Ok(SourceKind::Vgm),
                other => Err(anyhow!("unknown source extension .{}", other)),
            }
    }
}
