//! Source → Halotext conversion.
//!
//! One public entry point: `convert_file` takes any supported source and
//! returns a `Converted` bundle.  Nothing touches the disk except reading
//! the source.  Callers (`export`, `play`, `test`) write the bundle
//! themselves.

pub use crate::formats::flp::convert::{DepKind, ProbeReport};

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::formats::common::source::SourceKind;

// ── public API ─────────────────────────────────────────────────────────

pub use crate::conversion::*;

pub fn convert_file(src: &Path, opts: &ConvertOptions) -> Result<Converted> {
    match SourceKind::from_path(src)? {
        SourceKind::Flp => {
            let prj = crate::formats::flp::parse_file(src)?;
            crate::formats::flp::convert::convert(&prj, src, opts)
        }
        SourceKind::It => {
            let module = crate::formats::it::parse::parse_file(src)?;
            crate::formats::it::convert::convert(&module, src, opts)
        }
        SourceKind::Midi => {
            let mf = crate::formats::midi::parse::parse_file(src)?;
            crate::formats::midi::convert::convert(&mf, src, opts)
        }
        SourceKind::Vgm => crate::formats::vgm::convert::convert_vgm(src, opts), // <--- ADD THIS ARM
        SourceKind::Halotext => {
            let text = std::fs::read_to_string(src)?;
            crate::formats::halotext::parse::parse_str(&text)?;
            Ok(Converted {
                halotext: text,
                samples: Vec::new(),
               samples_json: String::new(),
               aux: Vec::new(),
            })
        }
    }
}

pub fn probe_file(src: &Path, opts: &ConvertOptions) -> Result<ProbeReport> {
    match SourceKind::from_path(src)? {
        SourceKind::Flp => crate::formats::flp::convert::probe(src, opts),
        _ => anyhow::bail!("check only supports .flp files (got {})", src.display()),
    }
}

pub fn output_stem(src: &Path) -> String {
    let filename = src.file_name().and_then(|s| s.to_str()).unwrap_or("output");
    let stem = if filename.to_ascii_lowercase().ends_with(crate::formats::halotext::spec::SOURCE_EXT) {
        &filename[..filename.len() - crate::formats::halotext::spec::SOURCE_EXT.len()]
    } else {
        src.file_stem().and_then(|s| s.to_str()).unwrap_or("output")
    };
    let mut out = String::with_capacity(stem.len());
    let mut prev_us = false;
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
            out.push(c);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    let t = out.trim_matches('_');
    if t.is_empty() { "output".into() } else { t.into() }
}

pub fn default_out_dir(src: &Path) -> PathBuf {
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let dir = src.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("{stem}_halotext"))
}
