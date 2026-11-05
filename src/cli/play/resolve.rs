//! Turning a source path into a playable halotext.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::archive;
use crate::pipe::{ ConvertOptions};
use crate::model::source::SourceKind;

use super::audio::Silence;
use super::cli::Args;

pub fn is_archive(path: &Path) -> bool {
    archive::is_archive_path(path)
}

// ── temporary bundle ───────────────────────────────────────────────────

pub struct TempBundle {
    dir: PathBuf,
}

impl TempBundle {
    pub fn new(tag: &str) -> Result<Self> {
        let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "circolate-{}-{}-{}",
            tag,
            std::process::id(),
                         nanos
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(TempBundle { dir })
    }

    pub fn write(&self, c: &crate::pipe::Converted, stem: &str) -> Result<PathBuf> {
        let ts_path = self.dir.join(format!("{}{}", stem, crate::spec::SOURCE_EXT));
        std::fs::write(&ts_path, &c.halotext)?;
        for s in &c.samples {
            let p = self.dir.join(&s.rel_path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, &s.wav)?;
        }
        for a in &c.aux {
            let p = self.dir.join(&a.rel_path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, &a.bytes)?;
        }
        if !c.samples_json.is_empty() {
            std::fs::write(self.dir.join("samples.json"), &c.samples_json)?;
        }
        Ok(ts_path)
    }
}

impl Drop for TempBundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ── resolution ────────────────────────────────────────────────────────

pub fn resolve_source(src: &Path, args: &Args) -> Result<(PathBuf, Option<TempBundle>)> {
    if is_archive(src) {
        return Ok((src.to_path_buf(), None));
    }
    match SourceKind::from_path(src)? {
        SourceKind::Halotext => Ok((src.to_path_buf(), None)),
        _ => {
            let mut opts = ConvertOptions::default();
            if let Some(d) = &args.samples_dir {
                opts.samples_dir = Some(d.clone());
            }
            if let Some(s) = &args.sf2_override {
                opts.sf2_path = Some(s.clone());
            }
            opts.skip_native = args.skip_native;

            let conv = {
                let _s = Silence::new();
                if let SourceKind::Vgm = SourceKind::from_path(src)? {
                    crate::pipe::vgm::convert_vgm(src, &opts)?
                } else {
                    crate::pipe::convert_file(src, &opts)?
                }
            };

            let tb = TempBundle::new("play")?;
            let stem = crate::pipe::output_stem(src);
            let ts = tb.write(&conv, &stem)?;
            if let Some(parent) = src.parent() {
                for name in ["cover.jpg", "cover.jpeg", "cover.png", "cover.webp"] {
                    let candidate = parent.join(name);
                    if candidate.is_file() {
                        let _ = std::fs::copy(&candidate, tb.dir.join(name));
                        break;
                    }
                }
            }
            Ok((ts, Some(tb)))
        }
    }
}
