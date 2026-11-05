//! Halotext archive format (.halo.zip).
//!
//! Layout:
//!   manifest.json
//!   halotext
//!   samples.json
//!   samples/*.wav        (may contain FLAC bytes — magic sniffed)
//!   samples/*.sf2
//!   instruments/*.instrument
//!   instruments/*.state
//!   fidelity.json
//!   cover.<ext>
//!   requires.json        (optional — plugin pointers)
//!   plugins/<name>.vst3/... (optional — embedded bundles)

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::asset::{AssetSource, ENTRY_KEY};
use crate::pipe::Converted;

/// Compound suffix used for newly written archives.
pub const EXTENSION: &str = "halo.zip";
pub const LEGACY_EXTENSIONS: &[&str] = &["hlz", "tsz"];

/// Recognize the compound current suffix and the prior single-extension names.
pub fn is_archive_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else { return false };
    let name = name.to_ascii_lowercase();
    if name.ends_with(".halo.zip") { return true; }
    path.extension().and_then(|s| s.to_str()).is_some_and(|ext| {
        LEGACY_EXTENSIONS.iter().any(|legacy| ext.eq_ignore_ascii_case(legacy))
    })
}
pub const MANIFEST_PATH: &str = "manifest.json";
pub const FORMAT_TAG: &str = "halotext-archive";
pub const FORMAT_VERSION: u32 = 1;

// ── manifest ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub tool: String,
    pub created: String,
    pub title: String,
    pub entry: String,
    pub sample_format: String,
    pub sample_rate: u32,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    #[serde(flatten)]
    pub kind: EntryKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryKind {
    Halotext,
    Sample {
        sample_id: u16,
        #[serde(default)]
        encoding: String,
        #[serde(default)]
        channels: u8,
        #[serde(default)]
        rate: u32,
    },
    Instrument,
    InstrumentState,
    Fidelity,
    SampleLibrary,
    Cover,
    Requires,
    Plugin {
        name: String,
        #[serde(default)]
        license: String,
    },
    Other,
}

// ── write options ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludePlugins {
    None,
    Free,
    All,
}

impl Default for IncludePlugins {
    fn default() -> Self { IncludePlugins::None }
}

#[derive(Debug, Clone, Copy)]
pub struct WriteOptions {
    pub include_plugins: IncludePlugins,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions { include_plugins: IncludePlugins::None }
    }
}

// ── shared bundle writers ───────────────────────────────────────────────

pub fn write_bundle_directory(
    out_dir: &Path,
    converted: &Converted,
    entry_path: &str,
    title: &str,
    sample_rate: u32,
    cover: Option<(&str, &[u8])>,
) -> Result<()> {
    let payload = crate::bundle::build_payload(converted, entry_path, title, sample_rate, cover)?;
    for file in &payload.files {
        let rel = Path::new(&file.relative_path);
        if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            bail!("bundle path escapes output directory: {}", file.relative_path);
        }
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        std::fs::write(&path, &file.data).with_context(|| format!("writing {}", path.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(&payload.manifest)?;
    std::fs::write(out_dir.join(MANIFEST_PATH), bytes)?;
    Ok(())
}

// ── archive writer ─────────────────────────────────────────────────────

pub fn write_archive(
    out: &Path,
    converted: &Converted,
    main_filename: &str,
    title: &str,
    sample_rate: u32,
    cover: Option<(&str, &[u8])>,
    opts: WriteOptions,
) -> Result<Manifest> {
    let mut payload = crate::bundle::build_payload(converted, main_filename, title, sample_rate, cover)?;

    if opts.include_plugins != IncludePlugins::None {
        for bundle in crate::deps::requires::bundles_from_aux(&converted.aux) {
            if opts.include_plugins == IncludePlugins::Free && !bundle.redistributable { continue; }
            let Some(path) = bundle.path.as_ref() else { continue };
            if !path.exists() { continue; }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let dir_name = format!("{}.vst3", stem);
            for (rel, bytes) in walk_plugin(path) {
                let relative_path = format!("plugins/{}/{}", dir_name, rel);
                payload.manifest.entries.push(Entry {
                    path: relative_path.clone(),
                    size: bytes.len() as u64,
                    sha256: sha256_hex(&bytes),
                    kind: EntryKind::Plugin { name: bundle.name.clone(), license: bundle.license.clone() },
                });
                payload.files.push(crate::bundle::BundleFile {
                    relative_path,
                    data: bytes,
                    kind: EntryKind::Plugin { name: bundle.name.clone(), license: bundle.license.clone() },
                });
            }
        }
    }

    let manifest_bytes = serde_json::to_vec_pretty(&payload.manifest)?;
    let file = File::create(out).with_context(|| format!("creating {}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file(MANIFEST_PATH, options)?;
    zip.write_all(&manifest_bytes)?;
    for entry in &payload.files {
        zip.start_file(&entry.relative_path, options)?;
        zip.write_all(&entry.data)?;
    }
    zip.finish()?;
    Ok(payload.manifest)
}

fn walk_plugin(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let Ok(md) = std::fs::symlink_metadata(root) else { return out };
    if md.file_type().is_file() {
        if let Ok(bytes) = std::fs::read(root) {
            let name = root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("plugin.vst3")
            .to_string();
            out.push((name, bytes));
        }
        return out;
    }
    walk_plugin_dir(root, root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn walk_plugin_dir(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
        if md.file_type().is_dir() {
            walk_plugin_dir(root, &p, out);
        } else if md.file_type().is_file() {
            if let Ok(rel) = p.strip_prefix(root) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if let Ok(bytes) = std::fs::read(&p) {
                    out.push((rel, bytes));
                }
            }
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

// ── read ───────────────────────────────────────────────────────────────

fn detect_prefix(zip: &mut zip::ZipArchive<File>) -> Option<String> {
    if zip.by_name(MANIFEST_PATH).is_ok() {
        return Some(String::new());
    }
    let mut best: Option<String> = None;
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index(i) else { continue };
        let name = entry.name();
        let Some(slash) = name.rfind('/') else { continue };
        let (head, tail) = name.split_at(slash + 1);
        if tail != MANIFEST_PATH {
            continue;
        }
        let head = head.to_string();
        if best.as_ref().map_or(true, |b| head.len() < b.len()) {
            best = Some(head);
        }
    }
    best
}

pub struct ArchiveReader {
    path: PathBuf,
    prefix: String,
    manifest: Manifest,
    index: HashMap<String, Entry>,
    zip: Mutex<Option<zip::ZipArchive<File>>>,
}

impl ArchiveReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let (prefix, manifest) = read_manifest_at(&path)?;
        let index: HashMap<String, Entry> = manifest
        .entries
        .iter()
        .map(|e| (e.path.clone(), e.clone()))
        .collect();
        Ok(ArchiveReader {
            path,
            prefix,
            manifest,
            index,
            zip: Mutex::new(None),
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn with_zip<R>(&self, f: impl FnOnce(&mut zip::ZipArchive<File>) -> Result<R>) -> Result<R> {
        let mut guard = self.zip.lock().unwrap();
        if guard.is_none() {
            let file = File::open(&self.path)
            .with_context(|| format!("reopening {}", self.path.display()))?;
            *guard = Some(zip::ZipArchive::new(file)?);
        }
        f(guard.as_mut().unwrap())
    }
}

impl AssetSource for ArchiveReader {
    fn read_bytes(&self, rel: &str) -> Result<Option<Vec<u8>>> {
        let key = if rel == ENTRY_KEY { self.manifest.entry.as_str() } else { rel };
        if !self.index.contains_key(key) { return Ok(None); }
        let zip_rel = format!("{}{}", self.prefix, key);
        self.with_zip(|zip| {
            let mut entry = zip
            .by_name(&zip_rel)
            .with_context(|| format!("entry {}", zip_rel))?;
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf)?;
            Ok(Some(buf))
        })
    }

    fn list(&self, prefix: &str, ext: Option<&str>) -> Vec<String> {
        let prefix = if prefix.is_empty() || prefix.ends_with('/') {
            prefix.to_string()
        } else {
            format!("{}/", prefix)
        };
        let mut out: Vec<String> = self
        .index
        .keys()
        .filter(|k| k.starts_with(&prefix))
        .filter(|k| !k[prefix.len()..].contains('/'))
        .filter(|k| match ext {
            Some(want) => k.rsplit('.').next() == Some(want),
                None => true,
        })
        .cloned()
        .collect();
        out.sort();
        out
    }

    fn list_recursive(&self, prefix: &str) -> Vec<String> {
        let prefix = if prefix.is_empty() || prefix.ends_with('/') {
            prefix.to_string()
        } else {
            format!("{}/", prefix)
        };
        let mut out: Vec<String> = self
        .index
        .keys()
        .filter(|k| k.starts_with(&prefix))
        .cloned()
        .collect();
        out.sort();
        out
    }

    fn cache_key(&self) -> Option<String> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.path.hash(&mut h);
        if let Ok(md) = std::fs::metadata(&self.path) {
            md.len().hash(&mut h);
            if let Ok(t) = md.modified() {
                if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
                    d.as_nanos().hash(&mut h);
                }
            }
        }
        Some(format!("{:016x}", h.finish()))
    }

    fn describe(&self) -> String {
        format!(
            "{} ({} entries{})",
                self.path.display(),
                self.manifest.entries.len(),
                if self.prefix.is_empty() {
                    String::new()
                } else {
                    format!(", prefix {:?}", self.prefix)
                },
        )
    }
}

pub fn read_manifest_at(path: &Path) -> Result<(String, Manifest)> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("not a zip archive")?;
    let prefix = detect_prefix(&mut zip)
    .context("not a halotext archive (no manifest.json)")?;
    let zip_path = format!("{}{}", prefix, MANIFEST_PATH);
    let mut entry = zip
    .by_name(&zip_path)
    .with_context(|| format!("opening {} inside archive", zip_path))?;
    let mut text = String::new();
    entry.read_to_string(&mut text)?;
    let manifest: Manifest = serde_json::from_str(&text).context("parsing manifest.json")?;
    if manifest.format != FORMAT_TAG {
        bail!("unexpected archive format: {}", manifest.format);
    }
    if manifest.version > FORMAT_VERSION {
        bail!(
            "archive version {} is newer than this build supports ({})",
              manifest.version, FORMAT_VERSION
        );
    }
    Ok((prefix, manifest))
}

pub fn read_manifest(path: &Path) -> Result<Manifest> {
    Ok(read_manifest_at(path)?.1)
}
