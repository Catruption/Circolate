//! The `Project` data structure and accessors.

pub mod assets;
pub mod loader;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::formats::flp::fidelity::Fidelity;
use crate::deps::requires::Requires;
use crate::formats::halotext::parse::{ShimEvent, HalotextEvent, HalotextPart};
use crate::formats::chip::patch::Patch;
use crate::hosting::chip::dac::DacTrack;
use crate::timing::TempoMap;

use super::inserts::ProjectInsertChain;
use super::lines::{LineCall, LineInfo};
use super::types::{
    MultiSampleInstrument, Sf2EngineSpec, Sf2Instrument, SlicexInstrument, VstInstrument,
};

pub(crate) use super::cache::Cache;
pub(crate) use super::loader::Loader;
pub(crate) use loader::load_project;
pub(crate) use super::types::{Sample, Sf2VoiceEnvelope};

#[derive(Debug, Clone)]
pub struct LyricSegment {
    pub time_sec: f64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct LyricLine {
    pub start_sec: f64,
    pub end_sec: f64,
    pub segments: Vec<LyricSegment>,
}

#[derive(Debug, Clone)]
pub(crate) struct VgmInstrument {
    pub(crate) iid: u16,
    pub(crate) file_bytes: Vec<u8>,
}

pub(crate) struct Project {
    pub(crate) source_dir: PathBuf,
    pub(crate) cover_path: Option<PathBuf>,
    pub(crate) fidelity: Option<Fidelity>,
    pub(crate) parts: HashMap<String, HalotextPart>,
    pub(crate) part_hash: HashMap<String, u64>,
    pub(crate) samples: HashMap<u32, Arc<Sample>>,
    pub(crate) path_to_id: HashMap<String, u32>,
    pub(crate) sf2_instruments: HashMap<String, Arc<Sf2Instrument>>,
    pub(crate) sf2_engine_specs: HashMap<String, Sf2EngineSpec>,
    pub(crate) vst_instruments: HashMap<String, Arc<VstInstrument>>,
    pub(crate) vgm_instruments: HashMap<String, Arc<VgmInstrument>>,
    pub(crate) vgm_dac: Option<DacTrack>,
    pub(crate) slicex_instruments: HashMap<String, Arc<SlicexInstrument>>,
    pub(crate) multi_sample_instruments: HashMap<String, Arc<MultiSampleInstrument>>,
    pub(crate) patch_instruments: HashMap<String, Patch>,
    pub(crate) patch_iids: HashMap<String, u16>,
    pub(crate) lines: Vec<LineInfo>,
    pub(crate) order: Vec<usize>,
    pub(crate) tempo: TempoMap,
    pub(crate) rows_per_measure: f64,
    pub(crate) render_end_sec: f64,
    pub(crate) total_samples: i64,
    pub(crate) song_volume: f64,
    pub(crate) linear_pan: bool,
    pub(crate) stereo_width: f32, // <--- NEW
    pub(crate) sample_rate: u32,
    pub(crate) title: String,
    pub(crate) initial_bpm: f64,
    pub(crate) time_signature: (u32, u32),
    pub(crate) voice_count: usize,
    pub(crate) requires: Option<Requires>,
    pub(crate) shims: Vec<ShimEvent>,
    pub(crate) lyrics: Vec<LyricLine>,
    pub(crate) chains: BTreeMap<u8, ProjectInsertChain>,
    pub(crate) channel_to_insert: BTreeMap<u16, u8>,
    note_rows_cache: OnceLock<Mutex<HashMap<String, Arc<Vec<super::notes::NoteRow>>>>>,
}

impl Project {
    pub(crate) fn line_count(&self) -> usize { self.lines.len() }
    pub(crate) fn slicex_for_part(&self, part_name: &str) -> Option<Arc<SlicexInstrument>> {
        let part = self.parts.get(part_name)?;
        self.slicex_instruments.get(&part.sample_path).cloned()
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
            vgm_instruments: HashMap::new(),
            vgm_dac: None,
            slicex_instruments: HashMap::new(),
            multi_sample_instruments: HashMap::new(),
            patch_instruments: HashMap::new(),
            patch_iids: HashMap::new(),
            lines: Vec::new(),
            order: Vec::new(),
            tempo: TempoMap::new(120.0, &[], 4),
            rows_per_measure: 4.0,
            render_end_sec: 0.0,
            total_samples: 0,
            song_volume: 1.0,
            linear_pan: false,
            stereo_width: 1.0, // <--- NEW
            sample_rate,
            title: "(no song)".into(),
            initial_bpm: 120.0,
            time_signature: (4, 4),
            voice_count: 0,
            requires: None,
            shims: Vec::new(),
            lyrics: Vec::new(),
            chains: BTreeMap::new(),
            channel_to_insert: BTreeMap::new(),
            note_rows_cache: OnceLock::new(),
        }
    }

    pub(crate) fn note_rows(&self, part_name: &str) -> Option<Arc<Vec<super::notes::NoteRow>>> {
        let part = self.parts.get(part_name)?;
        let cache = self.note_rows_cache.get_or_init(Default::default);
        if let Ok(mut cache) = cache.lock() {
            if let Some(rows) = cache.get(part_name) {
                return Some(Arc::clone(rows));
            }
            let rows = Arc::new(super::notes::split_notes(
                &part.events,
                self.part_subs_per_beat(part_name),
                self.part_sample_offset_u32(part_name),
            ));
            cache.insert(part_name.to_owned(), Arc::clone(&rows));
            Some(rows)
        } else {
            Some(Arc::new(super::notes::split_notes(
                &part.events,
                self.part_subs_per_beat(part_name),
                self.part_sample_offset_u32(part_name),
            )))
        }
    }

    pub(crate) fn sample_for_part(&self, part_name: &str) -> Option<Arc<Sample>> {
        let part = self.parts.get(part_name)?;
        let id = self.path_to_id.get(&part.sample_path)?;
        self.samples.get(id).cloned()
    }

    /// Preview sample for the waveform renderer.  Checks the WAV map
    /// first, then the SF2 map.  SF2 lookups use the composite key from
    /// `HalotextPart::sf2_key()` so flattened refs like
    /// `"samples/X.sf2"(6)` resolve to the preset-6 Sf2Instrument.
    pub(crate) fn preview_sample_for_part(&self, part_name: &str) -> Option<Arc<Sample>> {
        let part = self.parts.get(part_name)?;
        if let Some(id) = self.path_to_id.get(&part.sample_path) {
            if let Some(s) = self.samples.get(id) {
                return Some(s.clone());
            }
        }
        if let Some(inst) = self.sf2_instruments.get(&part.sf2_key()) {
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
        if let Some(spec) = self.sf2_engine_specs.get(&part.sf2_key()) {
            return Some(spec.iid);
        }
        if let Some(iid) = self.patch_iids.get(&part.sample_path) {
            return Some(*iid);
        }
        None
    }

    /// Look up the insert chain for a part.  Accepts either a plain
    /// path (WAV/VST/patch) or a bare SF2 path like `"samples/X.sf2"`.
    /// For SF2 parts, all presets from one sf2 file typically route to
    /// the same bus, so the first matching spec's iid is used.
    pub(crate) fn chain_for_part(&self, part_path: &str) -> Option<&ProjectInsertChain> {
        let iid = if let Some(inst) = self.vst_instruments.get(part_path) {
            inst.iid
        } else if let Some(spec) = self.sf2_engine_specs.get(part_path) {
            spec.iid
        } else if let Some(vgm) = self.vgm_instruments.get(part_path) {
            vgm.iid
        } else if let Some(pid) = self.patch_iids.get(part_path) {
            *pid
        } else if part_path.ends_with(".sf2") {
            // Composite-key fallback for flattened SF2 refs
            let prefix = format!("{}#", part_path);
            self.sf2_engine_specs
                .iter()
                .find(|(k, _)| k.starts_with(&prefix))
                .map(|(_, spec)| spec.iid)?
        } else {
            return None;
        };
        let bus = self.channel_to_insert.get(&iid)?;
        self.chains.get(bus)
    }

    pub(crate) fn patch_for_part(&self, part_name: &str) -> Option<&Patch> {
        let part = self.parts.get(part_name)?;
        self.patch_instruments.get(&part.sample_path)
    }

    pub(crate) fn patch_iid_for_part(&self, part_name: &str) -> Option<u16> {
        let part = self.parts.get(part_name)?;
        self.patch_iids.get(&part.sample_path).copied()
    }

    pub(crate) fn shims_for_line_path(&self, li: usize, path: &str) -> Vec<&ShimEvent> {
        let Some(line) = self.lines.get(li) else { return Vec::new(); };
        let call_indices: Vec<usize> = line.parts.iter().filter(|c| {
            self.parts.get(&c.name).map(|p| p.sample_path.as_str()) == Some(path)
        }).map(|c| c.call_index).collect();
        if call_indices.is_empty() { return Vec::new(); }

        let mut seen = std::collections::HashSet::new();
        let out: Vec<&ShimEvent> = self.shims.iter()
        .filter(|s| s.line == li)
        .filter(|s| s.target != "Tempo")
        .filter(|s| s.attached_call.map_or(false, |ci| call_indices.contains(&ci)))
        .filter(|s| seen.insert(s.target.clone()))
        .collect();
        out
    }

    pub(crate) fn cover_path(&self) -> Option<&Path> { self.cover_path.as_deref() }

    pub(crate) fn part_path(&self, name: &str) -> Option<&str> {
        self.parts.get(name).map(|p| p.sample_path.as_str())
    }

    /// Whether this part source has a volume channel we can drive.
    /// Accepts both legacy `.instrument` refs and flattened `.sf2(preset)`
    /// refs — for the latter, any composite `path#preset` entry counts.
    pub(crate) fn part_supports_volume(&self, part_path: &str) -> bool {
        if self.path_to_id.get(part_path).map(|id| self.samples.contains_key(id)).unwrap_or(false) { return true; }
        if self.vst_instruments.contains_key(part_path) { return true; }
        if self.sf2_engine_specs.contains_key(part_path) { return true; }
        if part_path.ends_with(".sf2") {
            let prefix = format!("{}#", part_path);
            if self.sf2_engine_specs.keys().any(|k| k.starts_with(&prefix)) { return true; }
        }
        if self.vgm_instruments.contains_key(part_path) { return true; }
        if self.patch_instruments.contains_key(part_path) { return true; }
        false
    }

    pub(crate) fn total_secs(&self) -> f64 { self.total_samples as f64 / self.sample_rate as f64 }

    pub(crate) fn line_part_names(&self, i: usize) -> Vec<&str> {
        self.lines.get(i).map(|l| l.parts.iter().map(|c| c.name.as_str()).collect()).unwrap_or_default()
    }

    pub(crate) fn line_calls(&self, i: usize) -> &[LineCall] {
        self.lines.get(i).map(|l| l.parts.as_slice()).unwrap_or(&[])
    }

    pub(crate) fn line_block_title(&self, i: usize, part_name: &str) -> Option<&str> {
        self.lines.get(i)?.parts.iter().find(|call| call.name == part_name).map(|call| call.block_title.as_str())
    }

    pub(crate) fn line_start_row(&self, li: usize) -> f64 {
        self.lines.get(li).map(|l| l.start_row).unwrap_or(0.0)
    }

    pub(crate) fn current_line(&self, pos: i64) -> Option<usize> {
        let mut lo = 0usize;
        let mut hi = self.order.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.lines[self.order[mid]].min_start <= pos { lo = mid + 1; } else { hi = mid; }
        }
        if lo == 0 { return None; }
        let li = self.order[lo - 1];
        if self.lines[li].max_end > pos { Some(li) } else { None }
    }

    pub(crate) fn rows_per_measure(&self) -> f64 { self.rows_per_measure }

    pub(crate) fn line_start_sample(&self, li: usize) -> i64 {
        self.lines.get(li).map(|l| l.min_start).unwrap_or(0)
    }

    pub(crate) fn line_end_sample(&self, li: usize) -> i64 {
        self.lines.get(li).map(|l| l.max_end).unwrap_or(0)
    }

    pub(crate) fn part_events(&self, name: &str) -> &[HalotextEvent] {
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

    pub(crate) fn part_hash_map(&self) -> &HashMap<String, u64> { &self.part_hash }

    pub(crate) fn line_measure_samples(&self, li: usize) -> i64 {
        let start_row = self.line_start_row(li);
        let end_row = start_row + self.rows_per_measure;
        let sr = self.sample_rate as f64;
        ((self.tempo.time_at_row(end_row) - self.tempo.time_at_row(start_row)) * sr) as i64
    }
}
