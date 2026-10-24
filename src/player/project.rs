//! Project loading and the `Project` data structure.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::asset::{AssetSource, DirSource, ENTRY_KEY};
use crate::audio_decode::decode_audio;
use crate::deps::index::PluginIndex;
use crate::fidelity::{Fidelity, FIDELITY_FILENAME};
use crate::deps::requires::{self, Requires};
use crate::formats::trackscript::{
    parse_str, ShimEvent, TrackscriptEvent, TrackscriptPart,
};
use crate::timing::TempoMap;

use super::cache::{fingerprint, hash_part};
use super::inserts::{self, ProjectInsertChain};
use super::lines::{resolve_line, tempo_shims_to_events, LineCall, LineInfo};
use super::types::{
    build_sf2_instrument, MultiSampleInstrument, Sf2EngineSpec, Sf2Instrument,
    Sf2InstrumentOverrides, VstInstrument,
};

pub(crate) use super::cache::Cache;
pub(crate) use super::loader::Loader;
pub(crate) use super::types::{Sample, Sf2VoiceEnvelope};

const TAIL_SECONDS: f64 = 0.05;

const VST_TRIM: f32 = 0.6;

const FALLBACK_VST_TRIM: f32 = 0.7;

// ── cover image ────────────────────────────────────────────────────────

fn cover_cache_dir() -> PathBuf {
    crate::paths::user_cache().join("covers")
}

fn find_cover_path(source: &dyn AssetSource) -> Option<PathBuf> {
    const PREFERRED: &[&str] = &[
        "cover.jpg", "cover.jpeg", "cover.png", "cover.webp", "cover.gif",
    ];

    let (name, bytes) = 'find: {
        for name in PREFERRED {
            if let Ok(Some(b)) = source.read_bytes(name) {
                break 'find (name.to_string(), b);
            }
        }
        for entry in source.list("", None) {
            if entry.contains('/') { continue; }
            let lower = entry.to_ascii_lowercase();
            let stem = lower.rsplit_once('.').map(|(s, _)| s).unwrap_or(&lower);
            if stem != "cover" { continue; }
            if let Ok(Some(b)) = source.read_bytes(&entry) {
                break 'find (entry, b);
            }
        }
        return None;
    };

    let ext = name.rsplit('.').next().unwrap_or("bin");
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish()
    };
    let dir = cover_cache_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{:016x}.{}", hash, ext));
    if !path.exists() {
        std::fs::write(&path, &bytes).ok()?;
    }
    Some(path)
}

fn fallback_cover_dir() -> Option<PathBuf> {
    let dir = crate::paths::system_data().join("cover");
    if dir.is_dir() { Some(dir) } else { None }
}

fn fallback_cover_path(source: &dyn AssetSource) -> Option<PathBuf> {
    use std::sync::{Mutex, OnceLock};
    static PICKS: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();

    let key = source
    .cache_key()
    .unwrap_or_else(|| source.describe().to_string());
    let mut picks = PICKS.get_or_init(Default::default).lock().ok()?;
    if let Some(prev) = picks.get(&key) {
        if prev.as_ref().map_or(true, |p| p.is_file()) {
            return prev.clone();
        }
    }
    let pick = fallback_cover_dir()
    .and_then(|d| crate::tui::widgets::cover::random_image_in(&d));
    picks.insert(key, pick.clone());
    pick
}

// ── embedded plugin cache ──────────────────────────────────────────────

fn plugins_cache_dir() -> PathBuf {
    crate::paths::user_cache().join("plugins")
}

fn extract_embedded_plugins(
    source: &dyn AssetSource,
) -> Result<HashMap<String, PathBuf>> {
    let entries = source.list_recursive("plugins");
    if entries.is_empty() {
        return Ok(HashMap::new());
    }

    let key = source.cache_key().unwrap_or_else(|| "anonymous".into());
    let cache = plugins_cache_dir().join(&key);

    if !cache.exists() {
        std::fs::create_dir_all(&cache)?;
        for rel in &entries {
            let Some(bytes) = source.read_bytes(rel)? else { continue };
            let dest = cache.join(rel);
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::write(&dest, &bytes)
            .with_context(|| format!("writing {}", dest.display()))?;
        }
    }

    let mut map = HashMap::new();
    let plugins_root = cache.join("plugins");
    if let Ok(rd) = std::fs::read_dir(&plugins_root) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.exists() { continue; }
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else { continue };
            let stem = name.strip_suffix(".vst3").unwrap_or(name).to_string();
            map.insert(stem, p);
        }
    }
    Ok(map)
}

// ── SF2 cache ──────────────────────────────────────────────────────────

fn sf2_cache_dir() -> PathBuf {
    crate::paths::user_cache().join("sf2")
}

fn materialize_sf2_file(source: &dyn AssetSource, rel: &str) -> Result<Option<PathBuf>> {
    let Some(bytes) = source.read_bytes(rel)? else {
        return Ok(None);
    };
    let key = source.cache_key().unwrap_or_else(|| "anonymous".into());
    let dest = sf2_cache_dir().join(&key).join(rel);

    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&dest, &bytes)
    .with_context(|| format!("writing {}", dest.display()))?;

    Ok(Some(dest))
}

#[allow(dead_code)]
fn materialize_sf2_reference(
    source: &dyn AssetSource,
    state_bytes: &mut Vec<u8>,
) -> Result<()> {
    let mut v: serde_json::Value = match serde_json::from_slice(state_bytes.as_slice()) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };

    let Some(fields) = v.get_mut("fields").and_then(|f| f.as_object_mut()) else {
        return Ok(());
    };
    let Some(file_val) = fields.get_mut("file") else { return Ok(()) };

    let outer_owned: String = match file_val.as_str() {
        Some(s) => s.to_string(),
        None => return Ok(()),
    };
    let inner: String = match serde_json::from_str(&outer_owned) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };

    if Path::new(&inner).exists() {
        return Ok(());
    }

    let rel = inner.trim_start_matches("./");
    let Some(bytes) = source.read_bytes(rel)? else {
        return Ok(());
    };

    let key = source.cache_key().unwrap_or_else(|| "anonymous".into());
    let dest = sf2_cache_dir().join(&key).join(rel);
    if !dest.exists() {
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&dest, &bytes)
        .with_context(|| format!("writing {}", dest.display()))?;
    }

    let abs = dest.canonicalize().unwrap_or(dest);
    let new_inner = abs.to_string_lossy().into_owned();

    *file_val = serde_json::Value::String(serde_json::to_string(&new_inner)?);
    *state_bytes = serde_json::to_vec(&v)?;
    Ok(())
}

// ── samples.json ───────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct SampleEntry {
    id: u32,
    file: Option<String>,
    #[serde(default)]
    loops: Loops,
}

#[derive(Deserialize, Debug, Default)]
struct Loops {
    main: Option<Loop>,
}

#[derive(Deserialize, Debug)]
struct Loop {
    #[serde(default)]
    start: u32,
    #[serde(default)]
    end: u32,
}

#[derive(Deserialize, Debug)]
struct SampleLibrary {
    #[serde(default)]
    samples: Vec<SampleEntry>,
}

// ── Project ────────────────────────────────────────────────────────────

pub(crate) struct Project {
    pub(crate) source_dir: PathBuf,
    cover_path: Option<PathBuf>,
    pub(crate) fidelity: Option<Fidelity>,
    pub(crate) parts: HashMap<String, TrackscriptPart>,
    pub(crate) part_hash: HashMap<String, u64>,
    pub(crate) samples: HashMap<u32, Arc<Sample>>,
    pub(crate) path_to_id: HashMap<String, u32>,
    pub(crate) sf2_instruments: HashMap<String, Arc<Sf2Instrument>>,
    pub(crate) sf2_engine_specs: HashMap<String, Sf2EngineSpec>,
    pub(crate) vst_instruments: HashMap<String, Arc<VstInstrument>>,
    pub(crate) multi_sample_instruments: HashMap<String, Arc<MultiSampleInstrument>>,
    pub(crate) lines: Vec<LineInfo>,
    pub(crate) order: Vec<usize>,
    pub(crate) tempo: TempoMap,
    pub(crate) rows_per_measure: f64,
    pub(crate) render_end_sec: f64,
    pub(crate) total_samples: i64,
    pub(crate) song_volume: f64,
    pub(crate) linear_pan: bool,
    pub(crate) sample_rate: u32,
    pub(crate) title: String,
    pub(crate) initial_bpm: f64,
    pub(crate) time_signature: (u32, u32),
    pub(crate) voice_count: usize,
    pub(crate) requires: Option<Requires>,
    pub(crate) shims: Vec<ShimEvent>,
    pub(crate) chains: BTreeMap<u8, ProjectInsertChain>,
    pub(crate) channel_to_insert: BTreeMap<u16, u8>,
}

impl Project {
    pub(crate) fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub(crate) fn empty(sample_rate: u32) -> Self {
        Project {
            source_dir: PathBuf::new(),
            cover_path: None,
            fidelity: None,
            parts: HashMap::new(),
            part_hash: HashMap::new(),
            samples: HashMap::new(),
            path_to_id: HashMap::new(),
            sf2_instruments: HashMap::new(),
            sf2_engine_specs: HashMap::new(),
            vst_instruments: HashMap::new(),
            multi_sample_instruments: HashMap::new(),
            lines: Vec::new(),
            order: Vec::new(),
            tempo: TempoMap::new(120.0, &[], 4),
            rows_per_measure: 4.0,
            render_end_sec: 0.0,
            total_samples: 0,
            song_volume: 1.0,
            linear_pan: false,
            sample_rate,
            title: "(no song)".into(),
            initial_bpm: 120.0,
            time_signature: (4, 4),
            voice_count: 0,
            requires: None,
            shims: Vec::new(),
            chains: BTreeMap::new(),
            channel_to_insert: BTreeMap::new(),
        }
    }

    pub(crate) fn sample_for_part(&self, part_name: &str) -> Option<Arc<Sample>> {
        let part = self.parts.get(part_name)?;
        let id = self.path_to_id.get(&part.sample_path)?;
        self.samples.get(id).cloned()
    }

    pub(crate) fn preview_sample_for_part(&self, part_name: &str) -> Option<Arc<Sample>> {
        let part = self.parts.get(part_name)?;

        if let Some(id) = self.path_to_id.get(&part.sample_path) {
            if let Some(s) = self.samples.get(id) {
                return Some(s.clone());
            }
        }

        if let Some(inst) = self.sf2_instruments.get(&part.sample_path) {
            let sid = inst.first_zone_sample_id()?;
            return self.samples.get(&sid).cloned();
        }

        None
    }

    pub(crate) fn iid_for_part(&self, part_name: &str) -> Option<u16> {
        let part = self.parts.get(part_name)?;

        if let Some(inst) = self.vst_instruments.get(&part.sample_path) {
            return Some(inst.iid);
        }
        if let Some(spec) = self.sf2_engine_specs.get(&part.sample_path) {
            return Some(spec.iid);
        }
        None
    }

    /// Walk part path → iid → insert bus → chain.  Returns `None` when
    /// the part has no chain or when `routing.json` was absent (the
    /// current converter doesn't emit it, so this is mostly `None` today).
    ///
    /// The **inline shim chains** that appear in every trackscript
    /// (`ƒpianoer(2)@<ƒFruity_Limiter()>@…`) are a separate data path —
    /// see `shims_for_line_path`.  `chain_for_part` is for the
    /// future `inserts/` sidecar format, which isn't emitted yet.
    pub(crate) fn chain_for_part(&self, part_path: &str) -> Option<&ProjectInsertChain> {
        let iid = if let Some(inst) = self.vst_instruments.get(part_path) {
            inst.iid
        } else if let Some(spec) = self.sf2_engine_specs.get(part_path) {
            spec.iid
        } else {
            return None;
        };
        let bus = self.channel_to_insert.get(&iid)?;
        self.chains.get(bus)
    }

    /// Inline shims attached to the given part path on the given
    /// BlockList line.  Filters out `Tempo` (those become tempo events,
    /// not display data) and dedupes by target name so `ƒFruity_Limiter()`
    /// appears once even if the same shim is duplicated across multiple
    /// LineCalls of the same part.
    ///
    /// This is the visual data source.  `p.shims` holds the parsed
    /// `@<ƒTarget()>` events; `attached_call` is an index into the
    /// BlockList line's `calls`, which `LineCall.call_index` matches.
    pub(crate) fn shims_for_line_path(
        &self,
        li: usize,
        path: &str,
    ) -> Vec<&ShimEvent> {
        let debug = std::env::var_os("SHIMS_DEBUG").is_some();
        let Some(line) = self.lines.get(li) else {
            if debug { eprintln!("shims: no line {li}"); }
            return Vec::new();
        };
        let call_indices: Vec<usize> = line
        .parts
        .iter()
        .filter(|c| {
            self.parts
            .get(&c.name)
            .map(|p| p.sample_path.as_str())
            == Some(path)
        })
        .map(|c| c.call_index)
        .collect();
        if debug {
            eprintln!(
                "shims: li={} path={:?} calls={:?} call_indices={:?}",
                li, path,
                line.parts.iter().map(|c| (c.name.as_str(), c.call_index)).collect::<Vec<_>>(),
                      call_indices,
            );
        }
        if call_indices.is_empty() {
            return Vec::new();
        }

        let mut seen = std::collections::HashSet::new();
        let out: Vec<&ShimEvent> = self
        .shims
        .iter()
        .filter(|s| s.line == li)
        .filter(|s| s.target != "Tempo")
        .filter(|s| {
            s.attached_call
            .map_or(false, |ci| call_indices.contains(&ci))
        })
        .filter(|s| seen.insert(s.target.clone()))
        .collect();
        if debug {
            eprintln!(
                "shims: {} match(es) on line {}: {:?}",
                      out.len(),
                      li,
                      out.iter().map(|s| s.target.as_str()).collect::<Vec<_>>(),
            );
        }
        out
    }

    pub(crate) fn cover_path(&self) -> Option<&Path> {
        self.cover_path.as_deref()
    }

    pub(crate) fn part_path(&self, name: &str) -> Option<&str> {
        self.parts.get(name).map(|p| p.sample_path.as_str())
    }

    pub(crate) fn part_supports_volume(&self, part_path: &str) -> bool {
        if self.path_to_id.get(part_path)
            .map(|id| self.samples.contains_key(id))
            .unwrap_or(false)
            {
                return true;
            }
            if self.vst_instruments.contains_key(part_path) {
                return true;
            }
            if self.sf2_engine_specs.contains_key(part_path) {
                return true;
            }
            false
    }

    pub(crate) fn total_secs(&self) -> f64 {
        self.total_samples as f64 / self.sample_rate as f64
    }

    pub(crate) fn line_part_names(&self, i: usize) -> Vec<&str> {
        self.lines
        .get(i)
        .map(|l| l.parts.iter().map(|c| c.name.as_str()).collect())
        .unwrap_or_default()
    }

    pub(crate) fn line_calls(&self, i: usize) -> &[LineCall] {
        self.lines.get(i).map(|l| l.parts.as_slice()).unwrap_or(&[])
    }

    pub(crate) fn line_block_title(&self, i: usize, part_name: &str) -> Option<&str> {
        self.lines
        .get(i)?
        .parts
        .iter()
        .find(|call| call.name == part_name)
        .map(|call| call.block_title.as_str())
    }

    pub(crate) fn line_start_row(&self, li: usize) -> f64 {
        self.lines.get(li).map(|l| l.start_row).unwrap_or(0.0)
    }

    pub(crate) fn current_line(&self, pos: i64) -> Option<usize> {
        let mut lo = 0usize;
        let mut hi = self.order.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.lines[self.order[mid]].min_start <= pos {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 { return None; }
        let li = self.order[lo - 1];
        if self.lines[li].max_end > pos { Some(li) } else { None }
    }

    pub(crate) fn rows_per_measure(&self) -> f64 {
        self.rows_per_measure
    }

    pub(crate) fn line_start_sample(&self, li: usize) -> i64 {
        self.lines.get(li).map(|l| l.min_start).unwrap_or(0)
    }

    pub(crate) fn line_end_sample(&self, li: usize) -> i64 {
        self.lines.get(li).map(|l| l.max_end).unwrap_or(0)
    }

    pub(crate) fn part_events(&self, name: &str) -> &[TrackscriptEvent] {
        self.parts.get(name).map(|p| p.events.as_slice()).unwrap_or(&[])
    }

    pub(crate) fn part_subs_per_beat(&self, name: &str) -> u32 {
        self.parts.get(name).map(|p| p.subs_per_beat).unwrap_or(4)
    }

    pub(crate) fn part_sample_offset(&self, name: &str) -> f64 {
        self.parts.get(name).map(|p| p.sample_offset as f64).unwrap_or(0.0)
    }

    pub(crate) fn part_sample_offset_u32(&self, name: &str) -> u32 {
        self.parts.get(name).map(|p| p.sample_offset).unwrap_or(0)
    }

    pub(crate) fn part_hash_map(&self) -> &HashMap<String, u64> {
        &self.part_hash
    }

    pub(crate) fn line_measure_samples(&self, li: usize) -> i64 {
        let start_row = self.line_start_row(li);
        let end_row = start_row + self.rows_per_measure;
        let sr = self.sample_rate as f64;
        ((self.tempo.time_at_row(end_row) - self.tempo.time_at_row(start_row)) * sr) as i64
    }
}

// ── load_project ───────────────────────────────────────────────────────

pub(crate) fn load_project_from_source(
    source: &dyn AssetSource,
    sample_rate: u32,
    use_cache: bool,
    quiet: bool,
    title_hint: Option<&str>,
) -> Result<(Arc<Project>, Cache)> {
    let ts_text = source
    .read_text(ENTRY_KEY)
    .context("reading trackscript")?
    .ok_or_else(|| anyhow::anyhow!("source has no trackscript: {}", source.describe()))?;
    let parsed = parse_str(&ts_text)?;
    let cfg = &parsed.config;

    let source_dir = PathBuf::new();
    let cover_path = find_cover_path(source).or_else(|| fallback_cover_path(source));

    let fidelity: Option<Fidelity> = match source.read_text(FIDELITY_FILENAME)? {
        Some(text) => match Fidelity::from_json(&text) {
            Ok(f) => {
                if !quiet {
                    println!(
                        "  fidelity: {} channels, {} automation lanes, {} playlist items, {} insert buses, {} multi-sample groups",
                        f.channels.len(), f.channel_automation.len(),
                             f.playlist.len(), f.inserts.len(), f.multi_sample.len(),
                    );
                }
                Some(f)
            }
            Err(e) => {
                if !quiet { eprintln!("  fidelity: {:#}, ignoring", e); }
                None
            }
        },
        None => None,
    };

    let embedded_plugins: HashMap<String, PathBuf> = if quiet {
        extract_embedded_plugins(source).unwrap_or_default()
    } else {
        match extract_embedded_plugins(source) {
            Ok(m) if !m.is_empty() => {
                println!("  extracted {} embedded plugin bundle(s)", m.len());
                m
            }
            Ok(m) => m,
            Err(e) => {
                eprintln!("  embedded plugin extraction failed: {:#}", e);
                HashMap::new()
            }
        }
    };

    let mut samples: HashMap<u32, Arc<Sample>> = HashMap::new();
    let mut fps: HashMap<u32, u64> = HashMap::new();
    let mut path_to_id: HashMap<String, u32> = HashMap::new();

    match source.read_text("samples.json")? {
        Some(text) if !text.trim().is_empty() => {
            let lib: SampleLibrary =
            serde_json::from_str(&text).context("parsing samples.json")?;
            for s in lib.samples {
                let Some(rel) = s.file else { continue };
                let Some(bytes) = source.read_bytes(&rel)? else { continue };
                let decoded = match decode_audio(&bytes, &rel) {
                    Ok(d) => d,
                    Err(e) => {
                        if !quiet {
                            eprintln!("  sample {} failed to decode: {:#}", rel, e);
                        }
                        continue;
                    }
                };
                let rate = decoded.rate;
                let stereo = decoded.channels_count() == 2;
                let mut chans = decoded.channels;
                let right = if stereo { chans.pop().unwrap() } else { Vec::new() };
                let left = chans.pop().unwrap_or_default();
                let loop_pts = s
                .loops
                .main
                .filter(|l| l.end > l.start)
                .map(|l| (l.start as i64, l.end as i64));
                let smp = Sample { left, right, stereo, rate, loop_pts };
                fps.insert(s.id, fingerprint(&smp));
                samples.insert(s.id, Arc::new(smp));
                if let Some(stripped) = rel.strip_prefix("./") {
                    path_to_id.insert(stripped.to_string(), s.id);
                }
                path_to_id.insert(rel, s.id);
            }
        }
        _ => {
            if !quiet {
                eprintln!("  (no samples.json in {})", source.describe());
            }
        }
    }
    if !quiet {
        println!("  loaded {} samples", samples.len());
    }

    let mut multi_sample_instruments: HashMap<String, Arc<MultiSampleInstrument>> =
    HashMap::new();
    if let Some(fid) = &fidelity {
        for (_iid_str, ms) in &fid.multi_sample {
            if ms.files.is_empty() { continue; }
            if let Some(inst) =
                MultiSampleInstrument::new(&ms.files, &ms.root_keys, &path_to_id)
                {
                    let primary = ms.files[0].clone();
                    multi_sample_instruments.insert(primary, Arc::new(inst));
                }
        }
    }
    if !quiet && !multi_sample_instruments.is_empty() {
        println!("  multi-sample: {} instrument(s)", multi_sample_instruments.len());
    }

    let mut sf2_instruments: HashMap<String, Arc<Sf2Instrument>> = HashMap::new();
    let mut sf2_engine_specs: HashMap<String, Sf2EngineSpec> = HashMap::new();
    let mut vst_instruments: HashMap<String, Arc<VstInstrument>> = HashMap::new();
    let mut next_sf2_id: u32 = 10_000;
    let mut vst_index: Option<PluginIndex> = None;

    for entry in source.list("instruments", Some("instrument")) {
        let rel = if entry.starts_with("instruments/") || entry.starts_with("./instruments/") {
            entry.trim_start_matches("./").to_string()
        } else {
            format!("instruments/{}", entry)
        };
        let Some(text) = source.read_text(&rel)? else {
            eprintln!("  (sidecar unreadable: {})", rel);
            continue;
        };
        let Ok(inst) = crate::deps::instruments::parse(&text, &rel) else { continue };
        let stem = rel
        .rsplit('/')
        .next()
        .and_then(|n| n.strip_suffix(".instrument"))
        .unwrap_or("instrument");
        let sidecar_rel = format!("instruments/{}.instrument", stem);

        if inst.is_sf2() {
            let sf2_rel = inst
            .sf2_path
            .to_string_lossy()
            .trim_start_matches("./")
            .to_string();

            let Some(sf2_bytes) = source.read_bytes(&sf2_rel)? else {
                if !quiet { eprintln!("  (SF2 {} not in source)", sf2_rel); }
                continue;
            };
            let sf2_on_disk = match materialize_sf2_file(source, &sf2_rel) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    if !quiet { eprintln!("  (SF2 {} not in source)", sf2_rel); }
                    continue;
                }
                Err(e) => {
                    if !quiet { eprintln!("  (SF2 {} materialize failed: {:#})", sf2_rel, e); }
                    continue;
                }
            };

            if let Ok(obj) = build_sf2_instrument(
                &sf2_bytes,
                inst.sf2_bank as u16,
                inst.sf2_preset as u16,
                next_sf2_id,
                &mut samples,
                if inst.slicex {
                    Some(Sf2InstrumentOverrides {
                        pan: inst.slicex_pan as f64,
                        gain: inst.slicex_gain,
                        tune_cents: inst.slicex_tune_cents,
                        env: Sf2VoiceEnvelope {
                            peak: 1.0,
                            delay_sec: inst.slicex_delay_sec,
                            attack_sec: inst.slicex_attack_sec,
                            hold_sec: inst.slicex_hold_sec,
                            decay_sec: inst.slicex_decay_sec,
                            sustain_fraction: inst.slicex_sustain_fraction,
                            release_sec: inst.slicex_release_sec,
                        },
                    })
                } else {
                    None
                },
            ) {
                next_sf2_id += obj.zone_count_hint() + 1;
                sf2_instruments.insert(sidecar_rel.clone(), Arc::new(obj));
            }

            sf2_engine_specs.insert(
                sidecar_rel,
                Sf2EngineSpec {
                    sf2_path: sf2_on_disk,
                    bank: inst.sf2_bank as u8,
                    preset: inst.sf2_preset as u8,
                    gain: inst.gain * if inst.slicex { inst.slicex_gain } else { 1.0 },
                    iid: inst.source_channel,
                },
            );
        } else if inst.is_vst3() {
            if inst.unresolved {
                continue;
            }

            let plugin_path = embedded_plugins
            .get(&inst.plugin)
            .cloned()
            .filter(|p| p.exists())
            .or_else(|| {
                let recorded = PathBuf::from(&inst.vst_path);
                if recorded.exists() { Some(recorded) } else { None }
            })
            .or_else(|| {
                let idx = vst_index.get_or_insert_with(PluginIndex::scan);
                idx.find_vst3(&inst.plugin).cloned()
            });

            let Some(plugin_path) = plugin_path else {
                if !quiet {
                    eprintln!(
                        "  (VST {:?} not installed, skipping; sidecar {})",
                              inst.plugin, sidecar_rel
                    );
                }
                continue;
            };

            let mut state_bytes = if inst.state_path.as_os_str().is_empty() {
                Vec::new()
            } else {
                let name = inst
                .state_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
                let state_rel = format!("instruments/{}", name);
                source.read_bytes(&state_rel)?.unwrap_or_default()
            };

            if inst.plugin == "SFLT" && !state_bytes.is_empty() {
                if let Err(e) = materialize_sf2_reference(source, &mut state_bytes) {
                    if !quiet {
                        eprintln!("  SF2 materialize failed for {}: {:#}", sidecar_rel, e);
                    }
                }
            }

            vst_instruments.insert(
                sidecar_rel,
                Arc::new(VstInstrument {
                    iid: inst.source_channel,
                    name: inst.plugin.clone(),
                         plugin_path,
                         state_bytes,
                         fallback: inst.fallback,
                         gain: inst.gain
                         * VST_TRIM
                         * if inst.fallback { FALLBACK_VST_TRIM } else { 1.0 },
                }),
            );
        }
    }
    let n_fallback = vst_instruments.values().filter(|v| v.fallback).count();
    if n_fallback > 1 {
        let spread = 1.0 / (n_fallback as f32).sqrt();
        for v in vst_instruments.values_mut() {
            if !v.fallback {
                continue;
            }
            let scaled = Arc::new(VstInstrument {
                iid: v.iid,
                name: v.name.clone(),
                                  plugin_path: v.plugin_path.clone(),
                                  state_bytes: v.state_bytes.clone(),
                                  fallback: v.fallback,
                                  gain: v.gain * spread,
            });
            *v = scaled;
        }
        if !quiet {
            println!(
                "  {} stand-in plugins: trimmed {:.1} dB each so they sit as backing",
                n_fallback,
                -20.0 * spread.log10(),
            );
        }
    }
    if !quiet {
        println!(
            "  loaded {} sf2 instruments, {} sf2 engines, {} vst instruments",
            sf2_instruments.len(),
                 sf2_engine_specs.len(),
                 vst_instruments.len()
        );
    }

    let requires: Option<Requires> = match source.read_text(requires::FILENAME)? {
        Some(text) => match requires::parse(&text) {
            Ok(r) => Some(r),
            Err(e) => {
                if !quiet {
                    eprintln!("  requires: {:#}, ignoring", e);
                }
                None
            }
        },
        None => None,
    };

    let (chains, channel_to_insert) = inserts::load_chain_metadata(source);
    if !quiet && !chains.is_empty() {
        let total: usize = chains.values().map(|c| c.slots.len()).sum();
        println!(
            "  loaded {} insert chain(s), {} slot(s) total",
                 chains.len(),
                 total
        );
    }

    let sr = sample_rate as f64;
    let rows_per_measure = cfg.time_signature.0 as f64 * cfg.subdivisions.max(1) as f64;

    let mut resolved_lines = Vec::with_capacity(parsed.block_list.len());
    let mut total_arrangement_rows: f64 = 0.0;
    for line in &parsed.block_list {
        let rl = resolve_line(line, &parsed.blocks, &parsed.parts, rows_per_measure);
        total_arrangement_rows += rl.duration;
        resolved_lines.push(rl);
    }

    let mut tempo_events = parsed.tempo_events.clone();
    let shim_tempo = tempo_shims_to_events(&parsed.shims, rows_per_measure);
    if !shim_tempo.is_empty() {
        if !quiet {
            println!("  tempo shims: {} event(s)", shim_tempo.len());
        }
        tempo_events.extend(shim_tempo);
        tempo_events.sort_by(|a, b| {
            a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    let tempo = TempoMap::new(cfg.bpm, &tempo_events, cfg.subdivisions);
    tempo.prepare(total_arrangement_rows);
    let render_end_sec = tempo.time_at_row(total_arrangement_rows) + TAIL_SECONDS;
    let total_samples = (render_end_sec * sr) as i64;

    let part_hash: HashMap<String, u64> = parsed
    .parts
    .iter()
    .map(|(n, p)| (n.clone(), hash_part(p, &path_to_id, &fps)))
    .collect();

    let mut cache = Cache::new(use_cache);
    let mut lines: Vec<LineInfo> = Vec::with_capacity(resolved_lines.len());
    let mut total_voices = 0usize;
    let mut cursor_row: f64 = 0.0;

    for rl in resolved_lines {
        let mut max_end_row = f64::NEG_INFINITY;

        for call in &rl.calls {
            let Some(part) = parsed.parts.get(&call.name) else { continue };
            let tpl = cache.template(
                part_hash[&call.name],
                part,
                &path_to_id,
                &samples,
                &sf2_instruments,
                &sf2_engine_specs,
                &vst_instruments,
                &multi_sample_instruments,
            );
            let measure_end = call.pattern_measure_start + call.pattern_measure_len;
            for tv in &tpl.voices {
                if tv.rel_row < call.pattern_measure_start || tv.rel_row >= measure_end {
                    continue;
                }
                let local = tv.rel_row - call.pattern_measure_start;
                let line_row = call.line_offset + local;
                max_end_row = max_end_row.max(line_row + tv.dur_rows);
                total_voices += 1;
            }
            for vn in &tpl.vst_notes {
                if vn.rel_row < call.pattern_measure_start || vn.rel_row >= measure_end {
                    continue;
                }
                let local = vn.rel_row - call.pattern_measure_start;
                let line_row = call.line_offset + local;
                max_end_row = max_end_row.max(line_row + vn.dur_rows);
                total_voices += 1;
            }
        }

        let this_start_row = cursor_row;
        let this_end_row = cursor_row + max_end_row.max(rl.duration);

        let (min_start, max_end) = (
            (tempo.time_at_row(this_start_row) * sr) as i64,
                                    (tempo.time_at_row(this_end_row.min(total_arrangement_rows)) * sr) as i64,
        );

        lines.push(LineInfo {
            parts: rl.calls,
            start_row: cursor_row,
            min_start,
            max_end,
        });

        cursor_row += rl.duration;
    }

    if total_voices == 0 {
        bail!("no playable voices (check samples.json and sample paths)");
    }

    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by_key(|&i| (lines[i].min_start, i));

    if !quiet {
        println!(
            "  parsed {} parts, {} blocks, {} BlockList lines, {} voices",
            parsed.parts.len(), parsed.blocks.len(),
                 parsed.block_list.len(), total_voices
        );
    }

    let title = title_hint
    .map(|s| s.to_string())
    .unwrap_or_else(|| "untitled".to_string());

    let project = Project {
        parts: parsed.parts,
        part_hash,
        samples,
        path_to_id,
        sf2_instruments,
        sf2_engine_specs,
        vst_instruments,
        multi_sample_instruments,
        lines,
        order,
        tempo,
        rows_per_measure,
        render_end_sec,
        total_samples,
        song_volume: cfg.song_volume / 100.0,
        linear_pan: cfg.panning_law == "linear",
        sample_rate,
        title,
        initial_bpm: cfg.bpm,
        time_signature: cfg.time_signature,
        voice_count: total_voices,
        source_dir,
        cover_path,
        fidelity,
        requires,
        shims: parsed.shims,
        chains,
        channel_to_insert,
    };
    Ok((Arc::new(project), cache))
}

// ── convenience wrappers ──────────────────────────────────────────────

pub(crate) fn load_project_from_dir(
    ts_path: &Path,
    sample_rate: u32,
    use_cache: bool,
    quiet: bool,
) -> Result<(Arc<Project>, Cache)> {
    let base = ts_path.parent().unwrap_or_else(|| Path::new("."));
    let name = ts_path
    .file_name()
    .and_then(|s| s.to_str())
    .unwrap_or("trackscript");
    let title = ts_path.file_stem().and_then(|s| s.to_str());
    let source = DirSource::new(base.to_path_buf(), name.to_string());
    load_project_from_source(&source, sample_rate, use_cache, quiet, title)
}

pub(crate) fn load_project_from_archive(
    archive_path: &Path,
    sample_rate: u32,
    use_cache: bool,
    quiet: bool,
) -> Result<(Arc<Project>, Cache)> {
    let reader = crate::archive::ArchiveReader::open(archive_path)?;
    let title = reader.manifest().title.clone();
    load_project_from_source(&reader, sample_rate, use_cache, quiet, Some(&title))
}

pub(crate) fn load_project(
    path: &Path,
    sample_rate: u32,
    use_cache: bool,
    quiet: bool,
) -> Result<(Arc<Project>, Cache)> {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case(crate::archive::EXTENSION) {
        load_project_from_archive(path, sample_rate, use_cache, quiet)
    } else {
        load_project_from_dir(path, sample_rate, use_cache, quiet)
    }
}
