//! Project loading and the `Project` data structure.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::asset::{AssetSource, DirSource, ENTRY_KEY};
use crate::audio_decode::decode_audio;
use crate::fidelity::{Fidelity, FIDELITY_FILENAME};
use crate::sources::trackscript::{parse_str, TrackscriptEvent, TrackscriptPart};
use crate::timing::TempoMap;

use super::cache::{fingerprint, hash_part};
use super::lines::{resolve_line, tempo_shims_to_events, LineCall, LineInfo};
use super::types::{
    build_sf2_instrument, MultiSampleInstrument, Sf2Instrument, VstInstrument,
};

pub(crate) use super::cache::Cache;
pub(crate) use super::loader::Loader;
pub(crate) use super::types::{Sample, Sf2VoiceEnvelope};

const TAIL_SECONDS: f64 = 0.05;

// ── cover image ────────────────────────────────────────────────────────

fn cover_cache_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("circolate").join("covers");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("circolate")
            .join("covers");
    }
    std::env::temp_dir().join("circolate-covers")
}

/// Find a cover image in the source under any name starting with
/// "cover." and materialize it to a cache file so callers that expect
/// a `Path` (the TUI) can use it.  Returns None if no cover is present.
fn find_cover_path(source: &dyn AssetSource) -> Option<PathBuf> {
    const PREFERRED: &[&str] = &[
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "cover.webp",
        "cover.gif",
    ];

    let (name, bytes) = 'find: {
        for name in PREFERRED {
            if let Ok(Some(b)) = source.read_bytes(name) {
                break 'find (name.to_string(), b);
            }
        }
        for entry in source.list("", None) {
            if entry.contains('/') {
                continue;
            }
            let lower = entry.to_ascii_lowercase();
            let stem = lower.rsplit_once('.').map(|(s, _)| s).unwrap_or(&lower);
            if stem != "cover" {
                continue;
            }
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
}

impl Project {
    pub(crate) fn line_count(&self) -> usize {
        self.lines.len()
    }


        /// A project with no content.  Used as the placeholder while the
    /// user hasn't picked a song yet — the queue widget is the entry
    /// point, and everything downstream (draw, engine, loader) just
    /// sees an empty arrangement.
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
        }
    }

    pub(crate) fn sample_for_part(&self, part_name: &str) -> Option<Arc<Sample>> {
        let part = self.parts.get(part_name)?;
        let id = self.path_to_id.get(&part.sample_path)?;
        self.samples.get(id).cloned()
    }

    pub(crate) fn iid_for_part(&self, part_name: &str) -> Option<u16> {
        let part = self.parts.get(part_name)?;
        let inst = self.vst_instruments.get(&part.sample_path)?;
        Some(inst.iid)
    }

    pub(crate) fn cover_path(&self) -> Option<&Path> {
        self.cover_path.as_deref()
    }

    pub(crate) fn part_path(&self, name: &str) -> Option<&str> {
        self.parts.get(name).map(|p| p.sample_path.as_str())
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
        if lo == 0 {
            return None;
        }
        let li = self.order[lo - 1];
        if self.lines[li].max_end > pos {
            Some(li)
        } else {
            None
        }
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
        self.parts
            .get(name)
            .map(|p| p.events.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn part_subs_per_beat(&self, name: &str) -> u32 {
        self.parts.get(name).map(|p| p.subs_per_beat).unwrap_or(4)
    }

    pub(crate) fn part_sample_offset(&self, name: &str) -> f64 {
        self.parts
            .get(name)
            .map(|p| p.sample_offset as f64)
            .unwrap_or(0.0)
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

// ── load_project (source-generic) ──────────────────────────────────────

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
    let debug = std::env::var_os("FLP_DEBUG").is_some();

    let source_dir = PathBuf::new();
    let cover_path = find_cover_path(source);

    // Fidelity sidecar.
    let fidelity: Option<Fidelity> = match source.read_text(FIDELITY_FILENAME)? {
        Some(text) => match Fidelity::from_json(&text) {
            Ok(f) => {
                if !quiet {
                    println!(
                        "  fidelity: {} channels, {} automation lanes, {} playlist items, {} insert buses, {} multi-sample groups",
                        f.channels.len(),
                        f.channel_automation.len(),
                        f.playlist.len(),
                        f.inserts.len(),
                        f.multi_sample.len(),
                    );
                }
                Some(f)
            }
            Err(e) => {
                if !quiet {
                    eprintln!("  fidelity: {:#}, ignoring", e);
                }
                None
            }
        },
        None => None,
    };

    // Samples.
    let mut samples: HashMap<u32, Arc<Sample>> = HashMap::new();
    let mut fps: HashMap<u32, u64> = HashMap::new();
    let mut path_to_id: HashMap<String, u32> = HashMap::new();

    match source.read_text("samples.json")? {
        Some(text) => {
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
                let right = if stereo {
                    chans.pop().unwrap()
                } else {
                    Vec::new()
                };
                let left = chans.pop().unwrap_or_default();
                let loop_pts = s
                    .loops
                    .main
                    .filter(|l| l.end > l.start)
                    .map(|l| (l.start as i64, l.end as i64));
                let smp = Sample {
                    left,
                    right,
                    stereo,
                    rate,
                    loop_pts,
                };
                fps.insert(s.id, fingerprint(&smp));
                samples.insert(s.id, Arc::new(smp));
                if let Some(stripped) = rel.strip_prefix("./") {
                    path_to_id.insert(stripped.to_string(), s.id);
                }
                path_to_id.insert(rel, s.id);
            }
        }
        None => {
            if !quiet {
                eprintln!("  (no samples.json in {})", source.describe());
            }
        }
    }
    if !quiet {
        println!("  loaded {} samples", samples.len());
    }

    if debug {
        let mut entries: Vec<(u32, String)> = path_to_id
            .iter()
            .map(|(k, v)| (*v, k.clone()))
            .collect();
        entries.sort();
        entries.dedup();
        eprintln!("flp: sample id map ({} entries):", entries.len());
        for (id, path) in entries.iter().take(500) {
            eprintln!("flp:   {:>6} <- {}", id, path);
        }
    }

    // Multi-sample instruments.
    let mut multi_sample_instruments: HashMap<String, Arc<MultiSampleInstrument>> =
        HashMap::new();
    if let Some(fid) = &fidelity {
        for (_iid_str, ms) in &fid.multi_sample {
            if ms.files.is_empty() {
                continue;
            }
            if let Some(inst) =
                MultiSampleInstrument::new(&ms.files, &ms.root_keys, &path_to_id)
            {
                let primary = ms.files[0].clone();
                multi_sample_instruments.insert(primary, Arc::new(inst));
            }
        }
    }
    if !quiet && !multi_sample_instruments.is_empty() {
        println!(
            "  multi-sample: {} instrument(s)",
            multi_sample_instruments.len()
        );
    }

    // Instrument sidecars.
    let mut sf2_instruments: HashMap<String, Arc<Sf2Instrument>> = HashMap::new();
    let mut vst_instruments: HashMap<String, Arc<VstInstrument>> = HashMap::new();
    let mut next_sf2_id: u32 = 10_000;

    for rel in source.list("instruments", Some("instrument")) {
        let Some(text) = source.read_text(&rel)? else { continue };
        let Ok(inst) = crate::instruments::parse(&text, &rel) else { continue };
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
                if !quiet {
                    eprintln!("  (SF2 {} not in source)", sf2_rel);
                }
                continue;
            };
            match build_sf2_instrument(
                &sf2_bytes,
                inst.sf2_bank as u16,
                inst.sf2_preset as u16,
                next_sf2_id,
                &mut samples,
            ) {
                Ok(obj) => {
                    next_sf2_id += obj.zone_count_hint() + 1;
                    sf2_instruments.insert(sidecar_rel, Arc::new(obj));
                }
                Err(e) => {
                    if !quiet {
                        eprintln!("  (SF2 {} failed: {:#})", sf2_rel, e);
                    }
                }
            }
        } else if inst.is_vst3() {
            if inst.unresolved || inst.vst_path.is_empty() {
                continue;
            }
            let plugin_path = PathBuf::from(&inst.vst_path);
            if !plugin_path.exists() {
                if !quiet {
                    eprintln!(
                        "  (VST sidecar points at missing file: {})",
                        plugin_path.display()
                    );
                }
                continue;
            }
            let state_bytes = if inst.state_path.as_os_str().is_empty() {
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
            vst_instruments.insert(
                sidecar_rel,
                Arc::new(VstInstrument {
                    iid: inst.source_channel,
                    plugin_path,
                    state_bytes,
                    fallback: inst.fallback,
                    gain: inst.gain,
                }),
            );
        }
    }
    if !quiet {
        println!(
            "  loaded {} sf2 instruments, {} vst instruments",
            sf2_instruments.len(),
            vst_instruments.len()
        );
    }

    // ── measure resolution ─────────────────────────────────────────
    let sr = sample_rate as f64;
    let rows_per_measure =
        cfg.time_signature.0 as f64 * cfg.subdivisions.max(1) as f64;

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
            parsed.parts.len(),
            parsed.blocks.len(),
            parsed.block_list.len(),
            total_voices
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

/// Dispatch by extension: `.tsz` → archive, anything else → directory.
/// This is what `play.rs` calls, and its signature matches the old
/// path-based `load_project`.
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