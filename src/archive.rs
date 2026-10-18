//! Trackscript archive format (.tsz).
//!
//! Layout:
//!   manifest.json
//!   trackscript
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
use crate::audio_decode::{decode_audio, DecodedAudio};
use crate::convert::{AuxFile, Converted, SampleOut};

pub const EXTENSION: &str = "tsz";
pub const MANIFEST_PATH: &str = "manifest.json";
pub const FORMAT_TAG: &str = "trackscript-archive";
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
    Trackscript,
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
    pub flac_max_ratio: f32,
    pub include_plugins: IncludePlugins,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            flac_max_ratio: 0.95,
            include_plugins: IncludePlugins::None,
        }
    }
}

// ── directory manifest writer ──────────────────────────────────────────

pub fn write_manifest_and_requires(
    out_dir: &Path,
    converted: &Converted,
    entry_path: &str,
    title: &str,
    sample_rate: u32,
    cover: Option<(&str, &[u8])>,
) -> Result<()> {
    let mut staged: Vec<(String, Vec<u8>, EntryKind)> = Vec::new();

    staged.push((
        entry_path.to_string(),
                 converted.trackscript.as_bytes().to_vec(),
                 EntryKind::Trackscript,
    ));

    let mut lib: serde_json::Value = if converted.samples_json.trim().is_empty() {
        serde_json::json!({
            "format": "trackscript-sample-library",
            "version": 2,
            "samples": [],
            "instruments": []
        })
    } else {
        serde_json::from_str(&converted.samples_json).context("parsing samples.json")?
    };

    if let Some(arr) = lib.get_mut("samples").and_then(|v| v.as_array_mut()) {
        for s in arr.iter_mut() {
            let Some(obj) = s.as_object_mut() else { continue };
            let id = obj.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let rel = obj.get("file").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if let Some(src) = converted.samples.iter().find(|x| x.rel_path == rel) {
                let decoded = decode_audio(&src.wav, &src.rel_path)?;
                staged.push((
                    src.rel_path.clone(),
                             src.wav.clone(),
                             EntryKind::Sample {
                                 sample_id: id,
                                 encoding: "wav".into(),
                             channels: decoded.channels_count() as u8,
                             rate: decoded.rate as u32,
                             },
                ));
            }
        }
    }

    staged.push((
        "samples.json".into(),
                 converted.samples_json.as_bytes().to_vec(),
                 EntryKind::SampleLibrary,
    ));

    for aux in &converted.aux {
        if aux.rel_path.starts_with("source/") {
            continue;
        }
        let (kind, _) = classify_aux(aux);
        staged.push((aux.rel_path.clone(), aux.bytes.clone(), kind));
    }

    if let Some((name, bytes)) = cover {
        staged.push((name.to_string(), bytes.to_vec(), EntryKind::Cover));
    }

    let requires = crate::deps::requires::build_from_aux(&converted.aux);
    let need_requires = !requires.plugins.is_empty() || !requires.fallbacks.is_empty();
    let mut requires_bytes = Vec::new();

    if need_requires {
        requires_bytes = serde_json::to_string_pretty(&requires)?.into_bytes();
        staged.push((
            crate::deps::requires::FILENAME.into(),
                     requires_bytes.clone(),
                     EntryKind::Requires,
        ));
    }

    let entries: Vec<Entry> = staged
    .into_iter()
    .map(|(path, bytes, kind)| Entry {
        path,
         size: bytes.len() as u64,
         sha256: sha256_hex(&bytes),
         kind,
    })
    .collect();

    let manifest = Manifest {
        format: FORMAT_TAG.into(),
            version: FORMAT_VERSION,
            tool: format!("circolate {}", env!("CARGO_PKG_VERSION")),
            created: now_iso8601(),
            title: title.into(),
            entry: entry_path.into(),
            sample_format: "wav".into(),
            sample_rate,
            entries,
    };

    if need_requires {
        let p = out_dir.join(crate::deps::requires::FILENAME);
        std::fs::write(&p, requires_bytes)?;
    }

    let manifest_bytes = serde_json::to_string_pretty(&manifest)?.into_bytes();
    let p = out_dir.join(MANIFEST_PATH);
    std::fs::write(&p, manifest_bytes)?;

    Ok(())
}

// ── archive writer ─────────────────────────────────────────────────────

pub fn write_archive(
    out: &Path,
    converted: &Converted,
    title: &str,
    sample_rate: u32,
    cover: Option<(&str, &[u8])>,
                     opts: WriteOptions,
) -> Result<Manifest> {
    let mut staged: Vec<(String, Vec<u8>, EntryKind, bool)> = Vec::new();

    staged.push((
        ENTRY_KEY.into(),
                 converted.trackscript.as_bytes().to_vec(),
                 EntryKind::Trackscript,
                 false,
    ));

    let mut lib: serde_json::Value = if converted.samples_json.trim().is_empty() {
        serde_json::json!({
            "format": "trackscript-sample-library",
            "version": 2,
            "samples": [],
            "instruments": []
        })
    } else {
        serde_json::from_str(&converted.samples_json)
        .context("parsing samples.json")?
    };
    let mut sample_format = "wav".to_string();

    if let Some(arr) = lib.get_mut("samples").and_then(|v| v.as_array_mut()) {
        for s in arr.iter_mut() {
            let Some(obj) = s.as_object_mut() else { continue };
            let id = obj.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let rel = obj.get("file").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let Some(src) = converted.samples.iter().find(|x| x.rel_path == rel) else {
                continue;
            };
            let enc = encode_sample(src, id, opts)?;
            if enc.used_flac {
                sample_format = "flac".into();
            }
            staged.push((src.rel_path.clone(), enc.bytes, enc.kind, enc.store_raw));
        }
    }

    staged.push((
        "samples.json".into(),
                 serde_json::to_string_pretty(&lib)?.into_bytes(),
                 EntryKind::SampleLibrary,
                 false,
    ));

    for aux in &converted.aux {
        if aux.rel_path.starts_with("source/") {
            continue;
        }
        let (kind, store) = classify_aux(aux);
        staged.push((aux.rel_path.clone(), aux.bytes.clone(), kind, store));
    }

    if let Some((name, bytes)) = cover {
        staged.push((name.to_string(), bytes.to_vec(), EntryKind::Cover, false));
    }

    let requires = crate::deps::requires::build_from_aux(&converted.aux);
    let need_requires_entry = !requires.plugins.is_empty() || !requires.fallbacks.is_empty();
    if need_requires_entry {
        let bytes = serde_json::to_string_pretty(&requires)?.into_bytes();
        staged.push((
            crate::deps::requires::FILENAME.into(),
                     bytes,
                     EntryKind::Requires,
                     false,
        ));
    }

    if opts.include_plugins != IncludePlugins::None {
        for bundle in crate::deps::requires::bundles_from_aux(&converted.aux) {
            if opts.include_plugins == IncludePlugins::Free && !bundle.redistributable {
                continue;
            }
            let Some(path) = bundle.path.as_ref() else { continue };
            if !path.exists() {
                continue;
            }
            let dir_name = if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                format!("{}.vst3", stem)
            } else {
                continue;
            };
            for (rel, bytes) in walk_plugin(path) {
                let entry = format!("plugins/{}/{}", dir_name, rel);
                staged.push((
                    entry,
                    bytes,
                    EntryKind::Plugin {
                        name: bundle.name.clone(),
                             license: bundle.license.clone(),
                    },
                    false,
                ));
            }
        }
    }

    let entries: Vec<Entry> = staged
    .iter()
    .map(|(p, b, k, _)| Entry {
        path: p.clone(),
         size: b.len() as u64,
         sha256: sha256_hex(b),
         kind: k.clone(),
    })
    .collect();

    let manifest = Manifest {
        format: FORMAT_TAG.into(),
            version: FORMAT_VERSION,
            tool: format!("circolate {}", env!("CARGO_PKG_VERSION")),
            created: now_iso8601(),
            title: title.into(),
            entry: ENTRY_KEY.into(),
            sample_format,
            sample_rate,
            entries,
    };
    let manifest_bytes = serde_json::to_string_pretty(&manifest)?.into_bytes();

    let file = File::create(out).with_context(|| format!("creating {}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let deflate = zip::write::SimpleFileOptions::default()
    .compression_method(zip::CompressionMethod::Deflated);
    let store = zip::write::SimpleFileOptions::default()
    .compression_method(zip::CompressionMethod::Stored);

    zip.start_file(MANIFEST_PATH, deflate)?;
    zip.write_all(&manifest_bytes)?;

    for (path, bytes, _k, raw) in &staged {
        let opt = if *raw { store } else { deflate };
        zip.start_file(path, opt)?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    Ok(manifest)
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

struct EncodedSample {
    bytes: Vec<u8>,
    kind: EntryKind,
    store_raw: bool,
    used_flac: bool,
}

fn encode_sample(src: &SampleOut, id: u16, opts: WriteOptions) -> Result<EncodedSample> {
    let decoded = decode_audio(&src.wav, &src.rel_path)?;
    let channels = decoded.channels_count() as u8;
    let rate = decoded.rate as u32;

    match encode_flac(&decoded) {
        Ok(flac) => {
            let ratio = flac.len() as f32 / src.wav.len() as f32;
            if ratio <= opts.flac_max_ratio {
                return Ok(EncodedSample {
                    bytes: flac,
                    kind: EntryKind::Sample {
                        sample_id: id,
                        encoding: "flac".into(),
                          channels,
                          rate,
                    },
                    store_raw: true,
                    used_flac: true,
                });
            } else {
                eprintln!(
                    "  {}: FLAC saved {:.1}% (threshold is {:.1}%), keeping WAV",
                          src.rel_path, (1.0 - ratio) * 100.0, (1.0 - opts.flac_max_ratio) * 100.0
                );
            }
        }
        Err(e) => {
            eprintln!("  {}: FLAC encoding failed: {:#}", src.rel_path, e);
        }
    }

    Ok(EncodedSample {
        bytes: src.wav.clone(),
       kind: EntryKind::Sample {
           sample_id: id,
           encoding: "wav".into(),
       channels,
       rate,
       },
       store_raw: false,
       used_flac: false,
    })
}

fn encode_flac(audio: &DecodedAudio) -> Result<Vec<u8>> {
    use std::process::Command;

    let n_ch = audio.channels_count();
    if n_ch == 0 || audio.frames() == 0 {
        bail!("no audio to encode");
    }
    let frames = audio.frames();
    let bits: u16 = audio.source_bits.max(16).min(24);
    let scale = (1i64 << (bits - 1)) as f32;
    let bytes_per_sample = ((bits + 7) / 8) as usize;

    let mut pcm: Vec<u8> = Vec::with_capacity(frames * n_ch * bytes_per_sample);
    for i in 0..frames {
        for c in 0..n_ch {
            let s = audio.channels[c][i].clamp(-1.0, 1.0);
            let v = (s * scale) as i64;
            let v = v.max(-(1i64 << (bits - 1))).min((1i64 << (bits - 1)) - 1);
            match bytes_per_sample {
                2 => pcm.extend_from_slice(&(v as i16).to_le_bytes()),
                3 => pcm.extend_from_slice(&(v as i32).to_le_bytes()[0..3]),
                4 => pcm.extend_from_slice(&(v as i32).to_le_bytes()),
                _ => bail!("unsupported bit depth {}", bits),
            }
        }
    }

    let data_len = pcm.len() as u32;
    let byte_rate = audio.rate as u32 * n_ch as u32 * bytes_per_sample as u32;
    let block_align = (n_ch as u16) * (bytes_per_sample as u16);
    let bits_u16 = bits as u16;

    let (fmt_tag, fmt_extra) = if bits > 16 {
        (0xFFFEu16, true)
    } else {
        (0x0001u16, false)
    };
    let fmt_size: u32 = if fmt_extra { 40 } else { 16 };

    let mut wav: Vec<u8> = Vec::with_capacity(12 + 8 + fmt_size as usize + 8 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(4 + 8 + fmt_size + 8 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&fmt_size.to_le_bytes());
    wav.extend_from_slice(&fmt_tag.to_le_bytes());
    wav.extend_from_slice(&(n_ch as u16).to_le_bytes());
    wav.extend_from_slice(&(audio.rate as u32).to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_u16.to_le_bytes());
    if fmt_extra {
        wav.extend_from_slice(&22u16.to_le_bytes());
        wav.extend_from_slice(&bits_u16.to_le_bytes());
        let channel_mask: u32 = match n_ch { 1 => 0x4, 2 => 0x3, _ => 0 };
        wav.extend_from_slice(&channel_mask.to_le_bytes());
        wav.extend_from_slice(&[
            0x01, 0x00, 0x00, 0x00,
            0x00, 0x00,
            0x10, 0x00,
            0x80, 0x00,
            0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ]);
    }
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&pcm);

    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let tmp = std::env::temp_dir();
    let in_path = tmp.join(format!("circolate-flac-{}-{}.wav", pid, seq));
    let out_path = tmp.join(format!("circolate-flac-{}-{}.flac", pid, seq));

    std::fs::write(&in_path, &wav)
    .with_context(|| format!("writing {}", in_path.display()))?;

    let status = Command::new("flac")
    .args(["--silent", "--force", "-o"])
    .arg(&out_path)
    .arg(&in_path)
    .status()
    .context("spawning flac — install the 'flac' package")?;

    let result = if status.success() {
        std::fs::read(&out_path)
        .with_context(|| format!("reading {}", out_path.display()))
    } else {
        Err(anyhow::anyhow!("flac exited with {}", status))
    };

    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
    result
}

fn classify_aux(aux: &AuxFile) -> (EntryKind, bool) {
    let p = aux.rel_path.as_str();
    if p == crate::fidelity::FIDELITY_FILENAME {
        (EntryKind::Fidelity, false)
    } else if p.starts_with("instruments/") && p.ends_with(".instrument") {
        (EntryKind::Instrument, false)
    } else if p.starts_with("instruments/") && p.ends_with(".state") {
        (EntryKind::InstrumentState, false)
    } else {
        (EntryKind::Other, false)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_secs() as i64)
    .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, m, s) = (rem / 3600, (rem / 60) % 60, rem % 60);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, m, s)
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
        if !self.index.contains_key(rel) {
            return Ok(None);
        }
        let zip_rel = format!("{}{}", self.prefix, rel);
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
    .context("not a trackscript archive (no manifest.json)")?;
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
