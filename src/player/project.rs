//! Project loading: samples.json + SF2 loading, part templates, `Project`,
//! and the streaming `Loader`.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::fidelity::{Fidelity, FIDELITY_FILENAME};
use crate::sf2::Sf2File;
use crate::sources::trackscript::{parse_file, TrackscriptEvent, TrackscriptPart, C5_HZ};
use crate::timing::{AutomationLane, TempoMap, Waypoint};

use super::engine::{gains, Scheduled, VstNote};

mod stretch;

const TAIL_SECONDS: f64 = 0.05;

use crate::sources::trackscript::ShimEvent;

/// Expand Tempo shim events into `(absolute_row, bpm, ramp_rows)` tuples
/// for `TempoMap`.  Each shim generates `count` steps; the ramp is 0
/// because a step change is instant at its row.  A later shim for the
/// same target overrides any earlier one from its start row onwards.
fn tempo_shims_to_events(
    shims: &[ShimEvent],
    rows_per_measure: f64,
) -> Vec<(f64, f64, f64)> {
    // Turn each shim into (absolute_row, shim_ref), sorted.
    let mut ordered: Vec<(f64, &ShimEvent)> = shims
    .iter()
    .filter(|s| s.target == "Tempo")
    .map(|s| (s.line as f64 * rows_per_measure + s.row, s))
    .collect();
    ordered.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out: Vec<(f64, f64, f64)> = Vec::new();

    for (base_row, shim) in ordered {
        // Later shim wins: drop any earlier events at or after this row.
        out.retain(|(r, _, _)| *r < base_row);

        // The shim's own start value is the tempo from here on.
        out.push((base_row, shim.start_value, 0.0));

        let count = shim.count.unwrap_or(1);
        let interval = if shim.interval_rows > 0.0 {
            shim.interval_rows
        } else {
            rows_per_measure  // defensive: no interval means "per measure"
        };

        let mut value = shim.start_value;
        for i in 0..count {
            let row = base_row + (i as f64 + 1.0) * interval;
            match shim.op.as_str() {
                "+" => value += shim.amount,
                "-" => value -= shim.amount,
                "*" => value *= shim.amount,
                "±" => {
                    // Triangle: alternate down/up around start_value.
                    let dir = if i % 2 == 0 { -1.0 } else { 1.0 };
                    value = shim.start_value + dir * shim.amount;
                }
                _ => {}
            }
            out.push((row, value, 0.0));
        }
    }

    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    out
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

// ── runtime data ───────────────────────────────────────────────────────

pub(crate) struct Sample {
    pub(crate) left: Vec<f32>,
    pub(crate) right: Vec<f32>,
    pub(crate) stereo: bool,
    pub(crate) rate: f64,
    pub(crate) loop_pts: Option<(i64, i64)>,
}

impl Sample {
    pub(crate) fn len(&self) -> usize {
        self.left.len()
    }
}

/// A VST3 instrument discovered in the project's `instruments/` directory.
/// One per channel that needs a synth.
pub(crate) struct VstInstrument {
    pub(crate) iid: u16,
    pub(crate) plugin_path: PathBuf,
    pub(crate) state_bytes: Vec<u8>,
    pub(crate) fallback: bool,
    /// Post-plugin output gain.  1.0 for exact matches.  Fallbacks use
    /// this to trim a replacement synth whose default patch is louder
    /// than the plugin it stands in for.
    pub(crate) gain: f32,
}

// ── SF2 instrument ─────────────────────────────────────────────────────

struct Sf2ZoneEntry {
    key_lo: u8,
    key_hi: u8,
    vel_lo: u8,
    vel_hi: u8,
    sample_id: u32,
    root_key: u8,
    tune_cents: i16,
}

struct Sf2Instrument {
    zones: Vec<Sf2ZoneEntry>,
}

impl Sf2Instrument {
    fn resolve(&self, key: u8, vel: u8) -> Option<(u32, f64)> {
        let key_dist = |z: &Sf2ZoneEntry| -> i32 {
            if key < z.key_lo {
                (z.key_lo - key) as i32
            } else if key > z.key_hi {
                (key - z.key_hi) as i32
            } else {
                0
            }
        };

        let mut best: Option<(&Sf2ZoneEntry, i32)> = None;
        for z in self.zones.iter().rev() {
            if vel < z.vel_lo || vel > z.vel_hi {
                continue;
            }
            let d = key_dist(z);
            if d == 0 {
                best = Some((z, 0));
                break;
            }
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((z, d));
            }
        }
        if best.is_none() {
            for z in self.zones.iter().rev() {
                let d = key_dist(z);
                if best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((z, d));
                }
            }
        }
        let (z, _) = best?;
        let semitones =
        (key as i32 - z.root_key as i32) as f64 + z.tune_cents as f64 / 100.0;
        let pitch_hz = C5_HZ * 2f64.powf(semitones / 12.0);
        Some((z.sample_id, pitch_hz))
    }
}

fn load_sf2_instrument(
    sf2_path: &Path,
    base_id: u32,
    samples: &mut HashMap<u32, Arc<Sample>>,
) -> Result<Sf2Instrument> {
    let sf2 = Sf2File::load(sf2_path)?;
    let mut zones: Vec<Sf2ZoneEntry> = Vec::with_capacity(sf2.zones.len());

    for (zi, z) in sf2.zones.iter().enumerate() {
        let sm = &sf2.samples[z.sample_idx];
        let start = (sm.start as i64 + z.start_offset as i64).clamp(0, sm.end as i64);
        let end = (sm.end as i64 + z.end_offset as i64).clamp(start, sf2.pcm.len() as i64);
        let ls = (sm.loop_start as i64 + z.loop_start_offset as i64).clamp(start, end);
        let le = (sm.loop_end as i64 + z.loop_end_offset as i64).clamp(ls, end);

        let mono: Vec<f32> = sf2.pcm[start as usize..end as usize]
        .iter()
        .map(|&s| s as f32 / 32768.0)
        .collect();

        let loop_pts = if sm.loop_mode != 0 && le > ls {
            Some(((ls - start) as i64, (le - start) as i64))
        } else {
            None
        };

        let arc = Arc::new(Sample {
            left: mono,
            right: Vec::new(),
                           stereo: false,
                           rate: sm.rate as f64,
                           loop_pts,
        });
        let sid = base_id + zi as u32;
        samples.insert(sid, arc);

        let root_key = z.root_override.unwrap_or(sm.root_key);
        zones.push(Sf2ZoneEntry {
            key_lo: z.key_lo,
            key_hi: z.key_hi,
            vel_lo: z.vel_lo,
            vel_hi: z.vel_hi,
            sample_id: sid,
            root_key,
            tune_cents: z.tune_cents,
        });
    }

    Ok(Sf2Instrument { zones })
}

// ── multi-sample instrument ────────────────────────────────────────────

struct MultiSampleZone {
    key_lo: u8,
    key_hi: u8,
    sample_id: u32,
    root_key: u8,
}

pub(crate) struct MultiSampleInstrument {
    zones: Vec<MultiSampleZone>,
}

impl MultiSampleInstrument {
    fn resolve(&self, key: u8) -> Option<(u32, f64)> {
        let mut best: Option<(&MultiSampleZone, i32)> = None;
        for z in &self.zones {
            if key >= z.key_lo && key <= z.key_hi {
                let semitones = (key as i32 - z.root_key as i32) as f64;
                let pitch_hz = C5_HZ * 2f64.powf(semitones / 12.0);
                return Some((z.sample_id, pitch_hz));
            }
            let d = if key < z.key_lo {
                z.key_lo as i32 - key as i32
            } else {
                key as i32 - z.key_hi as i32
            };
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((z, d));
            }
        }
        let (z, _) = best?;
        let semitones = (key as i32 - z.root_key as i32) as f64;
        let pitch_hz = C5_HZ * 2f64.powf(semitones / 12.0);
        Some((z.sample_id, pitch_hz))
    }
}

// ── templates ──────────────────────────────────────────────────────────

struct TVoice {
    rel_row: f64,
    dur_rows: f64,
    pitch_hz: f64,
    vol: f64,
    pan: f64,
    sample_id: u32,
    offset: u32,
    src_bpm: Option<f64>,
    pitch_wp: Vec<Waypoint>,
    vol_wp: Vec<Waypoint>,
    pan_wp: Vec<Waypoint>,
}

#[derive(Clone)]
struct VstNoteProto {
    rel_row: f64,
    dur_rows: f64,
    pitch_midi: u8,
    velocity: u8,
    iid: u16,
}

struct PartTemplate {
    voices: Vec<TVoice>,
    vst_notes: Vec<VstNoteProto>,
}

fn build_template(
    part: &TrackscriptPart,
    path_to_id: &HashMap<String, u32>,
    samples: &HashMap<u32, Arc<Sample>>,
    sf2_instruments: &HashMap<String, Arc<Sf2Instrument>>,
    vst_instruments: &HashMap<String, Arc<VstInstrument>>,
    multi_sample_instruments: &HashMap<String, Arc<MultiSampleInstrument>>,
) -> PartTemplate {
    let default_sid = path_to_id.get(&part.sample_path).copied();
    let subs = part.subs_per_beat as f64;
    let mut voices: Vec<TVoice> = Vec::new();
    let mut vst_notes: Vec<VstNoteProto> = Vec::new();
    let mut last: Option<usize> = None;

    for ev in &part.events {
        if !ev.is_waypoint() {
            let lookup_path = ev
            .sample_override
            .as_deref()
            .unwrap_or(&part.sample_path);

            let mut sid = default_sid;
            if let Some(p) = &ev.sample_override {
                sid = path_to_id.get(p).copied().or(default_sid);
            }

            let dur_beats = ev.dur_beats.filter(|d| *d != 0.0).unwrap_or(1.0);

            // Path 0: multi-sample instrument.
            if let Some(inst) = multi_sample_instruments.get(lookup_path) {
                let key = ev.pitch_midi.unwrap_or(60).clamp(0, 127) as u8;
                if let Some((sid, pitch_hz)) = inst.resolve(key) {
                    if samples.contains_key(&sid) {
                        voices.push(TVoice {
                            rel_row: ev.row,
                            dur_rows: dur_beats * subs,
                            pitch_hz,
                            vol: ev.vol.unwrap_or(100.0),
                            pan: ev.pan.unwrap_or(0.0),
                            sample_id: sid,
                            offset: ev.offset.unwrap_or(part.sample_offset),
                            src_bpm: None,
                            pitch_wp: Vec::new(),
                            vol_wp: Vec::new(),
                            pan_wp: Vec::new(),
                        });
                        last = Some(voices.len() - 1);
                        continue;
                    }
                }
            }

            // Path 1: direct single WAV sample.
            if let Some(sid) = sid {
                if samples.contains_key(&sid) {
                    voices.push(TVoice {
                        rel_row: ev.row,
                        dur_rows: dur_beats * subs,
                        pitch_hz: ev.pitch_hz.unwrap_or(C5_HZ),
                        vol: ev.vol.unwrap_or(100.0),
                        pan: ev.pan.unwrap_or(0.0),
                        sample_id: sid,
                        offset: ev.offset.unwrap_or(part.sample_offset),
                        src_bpm: ev.src_bpm.or(part.src_bpm),
                        pitch_wp: Vec::new(),
                        vol_wp: Vec::new(),
                        pan_wp: Vec::new(),
                    });
                    last = Some(voices.len() - 1);
                    continue;
                }
            }

            // Path 2: VST instrument sidecar.
            if let Some(inst) = vst_instruments.get(lookup_path) {
                let pitch = ev.pitch_midi.unwrap_or(60).clamp(0, 127) as u8;
                let vol_pct = ev.vol.unwrap_or(100.0).clamp(0.0, 100.0);
                let vel = (vol_pct * 127.0 / 100.0).round().clamp(1.0, 127.0) as u8;
                vst_notes.push(VstNoteProto {
                    rel_row: ev.row,
                    dur_rows: dur_beats * subs,
                    pitch_midi: pitch,
                    velocity: vel,
                    iid: inst.iid,
                });
                continue;
            }

            if std::env::var_os("SF2_TRACE").is_some() && sf2_instruments.contains_key(lookup_path) {
                eprintln!(
                    "sf2trace: part={} lookup={} key={:?} vol={:?}",
                    part.name, lookup_path, ev.pitch_midi, ev.vol
                );
            }
            if std::env::var_os("SF2_TRACE").is_some() && !sf2_instruments.is_empty() {
                if !sf2_instruments.contains_key(lookup_path) {
                    eprintln!(
                        "sf2trace: MISS part={} lookup={:?} keys={:?}",
                        part.name,
                        lookup_path,
                        sf2_instruments.keys().take(3).collect::<Vec<_>>()
                    );
                }
            }
            // Path 3: SF2 instrument sidecar.
            if let Some(inst) = sf2_instruments.get(lookup_path) {
                let key = ev.pitch_midi.unwrap_or(60).clamp(0, 127) as u8;
                let vel_pct = ev.vol.unwrap_or(100.0).clamp(0.0, 100.0);
                let vel = (vel_pct * 127.0 / 100.0).round().clamp(0.0, 127.0) as u8;
                if let Some((sid, pitch_hz)) = inst.resolve(key, vel) {
                    if samples.contains_key(&sid) {
                        voices.push(TVoice {
                            rel_row: ev.row,
                            dur_rows: dur_beats * subs,
                            pitch_hz,
                            vol: ev.vol.unwrap_or(100.0),
                            pan: ev.pan.unwrap_or(0.0),
                            sample_id: sid,
                            offset: ev.offset.unwrap_or(part.sample_offset),
                            src_bpm: None,
                            pitch_wp: Vec::new(),
                            vol_wp: Vec::new(),
                            pan_wp: Vec::new(),
                        });
                        last = Some(voices.len() - 1);
                        continue;
                    }
                }
            }

            continue;
        } else {
            let Some(idx) = last else { continue };
            let dur_rows = ev.dur_beats.unwrap_or(0.0) * subs;
            let v = &mut voices[idx];
            if let Some(hz) = ev.pitch_hz {
                v.pitch_wp.push((ev.row, dur_rows, hz));
            }
            if let Some(x) = ev.vol {
                v.vol_wp.push((ev.row, dur_rows, x));
            }
            if let Some(x) = ev.pan {
                v.pan_wp.push((ev.row, dur_rows, x));
            }
        }
    }
    PartTemplate { voices, vst_notes }
}

#[inline]
fn fnv(h: &mut u64, v: u64) {
    *h ^= v;
    *h = h.wrapping_mul(0x100000001b3);
}

fn fingerprint(s: &Sample) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    fnv(&mut h, s.rate.to_bits());
    fnv(&mut h, s.stereo as u64);
    match s.loop_pts {
        Some((a, b)) => {
            fnv(&mut h, a as u64);
            fnv(&mut h, b as u64);
        }
        None => fnv(&mut h, u64::MAX),
    }
    fnv(&mut h, s.left.len() as u64);
    for x in &s.left {
        fnv(&mut h, x.to_bits() as u64);
    }
    for x in &s.right {
        fnv(&mut h, x.to_bits() as u64);
    }
    h
}

fn hash_part(
    part: &TrackscriptPart,
    path_to_id: &HashMap<String, u32>,
    fps: &HashMap<u32, u64>,
) -> u64 {
    let mut h = DefaultHasher::new();
    let sample_key = |path: &str| -> (u32, u64) {
        match path_to_id.get(path) {
            Some(id) => (*id, fps.get(id).copied().unwrap_or(0)),
            None => (0, 0),
        }
    };
    part.sample_path.hash(&mut h);
    sample_key(&part.sample_path).hash(&mut h);
    part.sample_offset.hash(&mut h);
    part.src_bpm.map(f64::to_bits).hash(&mut h);
    part.subs_per_beat.hash(&mut h);
    part.beats_per_measure.hash(&mut h);
    let ob = |x: Option<f64>| x.map(f64::to_bits);
    for ev in &part.events {
        ev.op.hash(&mut h);
        ev.row.to_bits().hash(&mut h);
        ob(ev.pitch_hz).hash(&mut h);
        ob(ev.vol).hash(&mut h);
        ob(ev.pan).hash(&mut h);
        ob(ev.dur_beats).hash(&mut h);
        ev.offset.hash(&mut h);
        ob(ev.src_bpm).hash(&mut h);
        ev.sample_override.hash(&mut h);
        if let Some(p) = &ev.sample_override {
            sample_key(p).hash(&mut h);
        }
    }
    h.finish()
}

pub(crate) struct Cache {
    enabled: bool,
    map: HashMap<u64, Arc<PartTemplate>>,
    stretched: HashMap<(u32, u64), Arc<Sample>>,
    hits: u64,
    misses: u64,
}

impl Cache {
    fn new(enabled: bool) -> Self {
        Cache {
            enabled,
            map: HashMap::new(),
            stretched: HashMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    fn template(
        &mut self,
        hash: u64,
        part: &TrackscriptPart,
        path_to_id: &HashMap<String, u32>,
        samples: &HashMap<u32, Arc<Sample>>,
        sf2_instruments: &HashMap<String, Arc<Sf2Instrument>>,
        vst_instruments: &HashMap<String, Arc<VstInstrument>>,
        multi_sample_instruments: &HashMap<String, Arc<MultiSampleInstrument>>,
    ) -> Arc<PartTemplate> {
        if self.enabled {
            if let Some(t) = self.map.get(&hash) {
                self.hits += 1;
                return Arc::clone(t);
            }
        }
        self.misses += 1;
        let t = Arc::new(build_template(
            part,
            path_to_id,
            samples,
            sf2_instruments,
            vst_instruments,
            multi_sample_instruments,
        ));
        if self.enabled {
            self.map.insert(hash, Arc::clone(&t));
        }
        t
    }

    fn stretched(&mut self, sid: u32, sample: &Arc<Sample>, factor: f64) -> Arc<Sample> {
        let key = (sid, factor.to_bits());
        if let Some(s) = self.stretched.get(&key) {
            return Arc::clone(s);
        }
        let mut chans: Vec<&[f32]> = vec![&sample.left];
        if sample.stereo {
            chans.push(&sample.right);
        }
        let mut out = stretch::stretch(&chans, factor, sample.rate).into_iter();
        let left = out.next().unwrap_or_default();
        let right = out.next().unwrap_or_else(|| left.clone());
        let scale = |p: i64| (p as f64 * factor).round() as i64;
        let s = Arc::new(Sample {
            left,
            right,
            stereo: sample.stereo,
            rate: sample.rate,
            loop_pts: sample.loop_pts.map(|(a, b)| (scale(a), scale(b))),
        });
        self.stretched.insert(key, Arc::clone(&s));
        s
    }

    pub(crate) fn describe(&self) -> String {
        if self.enabled {
            format!("part cache: {} hits, {} builds", self.hits, self.misses)
        } else {
            format!("part cache: off ({} builds)", self.misses)
        }
    }
}

// ── project ────────────────────────────────────────────────────────────

struct LineInfo {
    parts: Vec<String>,
    min_start: i64,
    max_end: i64,
}

pub(crate) struct Project {
    pub(crate) source_dir: PathBuf,
    cover_path: Option<PathBuf>,
    pub(crate) fidelity: Option<Fidelity>,
    parts: HashMap<String, TrackscriptPart>,
    part_hash: HashMap<String, u64>,
    samples: HashMap<u32, Arc<Sample>>,
    path_to_id: HashMap<String, u32>,
    sf2_instruments: HashMap<String, Arc<Sf2Instrument>>,
    pub(crate) vst_instruments: HashMap<String, Arc<VstInstrument>>,
    multi_sample_instruments: HashMap<String, Arc<MultiSampleInstrument>>,
    lines: Vec<LineInfo>,
    order: Vec<usize>,
    tempo: TempoMap,
    rows_per_measure: f64,
    render_end_sec: f64,
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

    pub(crate) fn line_parts(&self, i: usize) -> &[String] {
        self.lines.get(i).map(|l| l.parts.as_slice()).unwrap_or(&[])
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

    /// Same as `part_sample_offset`, but in the native `u32` type so
    /// callers that need the raw value for offset arithmetic (the
    /// tracker's sample playhead, the static synth envelope) don't have
    /// to round-trip through `f64`.
    pub(crate) fn part_sample_offset_u32(&self, name: &str) -> u32 {
        self.parts.get(name).map(|p| p.sample_offset).unwrap_or(0)
    }

    /// The part hash map, keyed by part name.  `u64` fingerprints of
    /// each part's full event stream, computed at load time and used
    /// by the file watcher to tell "the file changed on disk but the
    /// music didn't" apart from a genuine edit.
    pub(crate) fn part_hash_map(&self) -> &HashMap<String, u64> {
        &self.part_hash
    }

    pub(crate) fn line_measure_samples(&self, li: usize) -> i64 {
        let start_row = li as f64 * self.rows_per_measure;
        let end_row = start_row + self.rows_per_measure;
        let sr = self.sample_rate as f64;
        ((self.tempo.time_at_row(end_row) - self.tempo.time_at_row(start_row)) * sr) as i64
    }
}

fn load_wav(path: &Path) -> Result<(Vec<Vec<f32>>, f64)> {
    let mut reader =
    hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
            .samples::<i32>()
            .map(|s| s.map(|v| v as f32 / max))
            .collect::<Result<_, _>>()?
        }
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
    };
    let keep = ch.min(2);
    let mut chans = vec![Vec::with_capacity(interleaved.len() / ch); keep];
    for frame in interleaved.chunks_exact(ch) {
        for (c, out) in chans.iter_mut().enumerate() {
            out.push(frame[c]);
        }
    }
    Ok((chans, spec.sample_rate as f64))
}

pub(crate) fn load_project(
    ts_path: &Path,
    sample_rate: u32,
    use_cache: bool,
    quiet: bool,
) -> Result<(Arc<Project>, Cache)> {
    let parsed = parse_file(ts_path)?;
    let base = ts_path.parent().unwrap_or_else(|| Path::new("."));
    let cfg = &parsed.config;
    let debug = std::env::var_os("FLP_DEBUG").is_some();

    let source_dir = base.to_path_buf();
    let cover_path = {
        let p = source_dir.join("cover.jpg");
        if p.exists() { Some(p) } else { None }
    };

    // Fidelity sidecar is read first because it tells us which channels
    // are multi-sample.
    let fidelity: Option<Fidelity> = {
        let p = base.join(FIDELITY_FILENAME);
        if p.exists() {
            match std::fs::read_to_string(&p)
            .with_context(|| format!("reading {}", p.display()))
            .and_then(|text| {
                Fidelity::from_json(&text)
                .with_context(|| format!("parsing {}", p.display()))
            }) {
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
            }
        } else {
            None
        }
    };

    let mut samples: HashMap<u32, Arc<Sample>> = HashMap::new();
    let mut fps: HashMap<u32, u64> = HashMap::new();
    let mut path_to_id: HashMap<String, u32> = HashMap::new();
    let lib_path = base.join("samples.json");
    if lib_path.exists() {
        let text = std::fs::read_to_string(&lib_path)?;
        let lib: SampleLibrary = serde_json::from_str(&text)
        .with_context(|| format!("parsing {}", lib_path.display()))?;
        for s in lib.samples {
            let Some(rel) = s.file else { continue };
            let path = base.join(&rel);
            if !path.exists() {
                continue;
            }
            let (mut chans, rate) = load_wav(&path)?;
            let stereo = chans.len() == 2;
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
    } else {
        if !quiet {
            eprintln!("  (no samples.json in {})", base.display());
        }
    }
    if !quiet {
        println!("  loaded {} samples", samples.len());
    }

    // Sample id → path mapping.  Useful when chasing "why is this one
    // sample silent": grep the debug output for the file's stem.
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

    // ── Multi-sample instruments ──────────────────────────────────────
    let mut multi_sample_instruments: HashMap<String, Arc<MultiSampleInstrument>> = HashMap::new();
    if let Some(fid) = &fidelity {
        for (_iid_str, ms) in &fid.multi_sample {
            if ms.files.is_empty() {
                continue;
            }
            let n = ms.files.len() as u32;
            let mut zones: Vec<MultiSampleZone> = Vec::with_capacity(ms.files.len());
            for (zi, file) in ms.files.iter().enumerate() {
                let Some(&sid) = path_to_id.get(file) else { continue };
                let root = ms
                .root_keys
                .get(zi)
                .copied()
                .flatten()
                .unwrap_or(60)
                .clamp(0, 127) as u8;
                let lo = ((zi as u32) * 128 / n) as u8;
                let hi = (((zi as u32 + 1) * 128 / n).saturating_sub(1)).min(127) as u8;
                zones.push(MultiSampleZone {
                    key_lo: lo,
                    key_hi: hi,
                    sample_id: sid,
                    root_key: root,
                });
            }
            if zones.is_empty() {
                continue;
            }
            let primary = ms.files[0].clone();
            multi_sample_instruments.insert(primary, Arc::new(MultiSampleInstrument { zones }));
        }
    }
    if !quiet && !multi_sample_instruments.is_empty() {
        println!(
            "  multi-sample: {} instrument(s)",
            multi_sample_instruments.len()
        );
    }

    // ── Load instrument sidecars ──────────────────────────────────────
    let mut sf2_instruments: HashMap<String, Arc<Sf2Instrument>> = HashMap::new();
    let mut vst_instruments: HashMap<String, Arc<VstInstrument>> = HashMap::new();
    let mut next_sf2_id: u32 = 10_000;
    let inst_dir = base.join("instruments");
    if let Ok(entries) = std::fs::read_dir(&inst_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("instrument") {
                continue;
            }
            let Ok(inst) = crate::instruments::load(&p) else { continue };
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            let rel = format!("instruments/{}.instrument", stem);

            if inst.is_sf2() {
                let sf2_path = if inst.sf2_path.is_absolute() {
                    inst.sf2_path.clone()
                } else {
                    base.join(&inst.sf2_path)
                };
                match load_sf2_instrument(&sf2_path, next_sf2_id, &mut samples) {
                    Ok(obj) => {
                        next_sf2_id += obj.zones.len() as u32 + 1;
                        sf2_instruments.insert(rel, Arc::new(obj));
                    }
                    Err(e) => {
                        if !quiet {
                            eprintln!(
                                "  (failed to load SF2 {}: {:#})",
                                sf2_path.display(),
                                e
                            );
                        }
                    }
                }
            } else if inst.is_vst3() {
                if inst.unresolved {
                    if !quiet {
                        eprintln!("  (skipping unresolved VST: {})", p.display());
                    }
                    continue;
                }
                if inst.vst_path.is_empty() {
                    if !quiet {
                        eprintln!("  (VST sidecar has no Path: {})", p.display());
                    }
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
                } else if inst.state_path.exists() {
                    std::fs::read(&inst.state_path).unwrap_or_default()
                } else {
                    Vec::new()
                };
                vst_instruments.insert(
                    rel,
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
    }
    if !quiet {
        println!(
            "  loaded {} sf2 instruments, {} vst instruments",
            sf2_instruments.len(),
            vst_instruments.len()
        );
    }

    let sr = sample_rate as f64;
    let rows_per_measure = cfg.time_signature.0 as f64 * cfg.subdivisions.max(1) as f64;

    let mut tempo_events = parsed.tempo_events.clone();
    let shim_tempo = tempo_shims_to_events(&parsed.shims, rows_per_measure);
    if !shim_tempo.is_empty() {
        if !quiet {
            println!("  tempo shims: {} event(s)", shim_tempo.len());
        }
        tempo_events.extend(shim_tempo);
        tempo_events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    }

    let tempo = TempoMap::new(cfg.bpm, &tempo_events, cfg.subdivisions);
    let total_rows = parsed.block_list.len() as f64 * rows_per_measure;
    tempo.prepare(total_rows);
    let render_end_sec = tempo.time_at_row(total_rows) + TAIL_SECONDS;
    let total_samples = (render_end_sec * sr) as i64;

    let part_hash: HashMap<String, u64> = parsed
    .parts
    .iter()
    .map(|(n, p)| (n.clone(), hash_part(p, &path_to_id, &fps)))
    .collect();

    let mut cache = Cache::new(use_cache);
    let mut lines: Vec<LineInfo> = Vec::with_capacity(parsed.block_list.len());
    let mut total_voices = 0usize;
    for (li, line) in parsed.block_list.iter().enumerate() {
        let placement_row = li as f64 * rows_per_measure;
        let mut names = Vec::new();
        let (mut min_row, mut max_end_row) = (f64::INFINITY, f64::NEG_INFINITY);
        for call in line {
            if std::env::var_os("TS_DEBUG").is_some() && call.contains("ae2f") {
                eprintln!(
                    "ts: block call {:?} parts.contains_key={}",
                    call,
                    parsed.parts.contains_key(call)
                );
            }
            let part_names: Vec<String> = parsed
            .blocks
            .get(call)
            .cloned()
            .unwrap_or_else(|| vec![call.clone()]);
            for name in part_names {
                if std::env::var_os("TS_DEBUG").is_some() && name.contains("ae2f") {
                    eprintln!("ts: resolved part name {:?}", name);
                }
                let Some(part) = parsed.parts.get(&name) else {
                    if std::env::var_os("TS_DEBUG").is_some() && name.contains("ae2f") {
                        eprintln!("ts: NO PART for {:?}", name);
                    }
                    continue;
                };
                let tpl = cache.template(
                    part_hash[&name],
                    part,
                    &path_to_id,
                    &samples,
                    &sf2_instruments,
                    &vst_instruments,
                    &multi_sample_instruments,
                );
                if std::env::var_os("TS_DEBUG").is_some() && name.contains("ae2f") {
                    eprintln!(
                        "ts: template voices={} vst_notes={}",
                        tpl.voices.len(),
                        tpl.vst_notes.len()
                    );
                }
                for tv in &tpl.voices {
                    let start_row = placement_row + tv.rel_row;
                    min_row = min_row.min(start_row);
                    max_end_row = max_end_row.max(start_row + tv.dur_rows);
                    total_voices += 1;
                }
                for vn in &tpl.vst_notes {
                    let start_row = placement_row + vn.rel_row;
                    min_row = min_row.min(start_row);
                    max_end_row = max_end_row.max(start_row + vn.dur_rows);
                    total_voices += 1;
                }
                names.push(name);
            }
        }
        let line_end_row = placement_row + rows_per_measure;
        let (min_start, max_end) = (
            (tempo.time_at_row(placement_row) * sr) as i64,
            (tempo.time_at_row(max_end_row.max(line_end_row))
            .min(render_end_sec) * sr) as i64,
        );
        lines.push(LineInfo {
            parts: names,
            min_start,
            max_end,
        });
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

    if std::env::var_os("TRACE_OFFSET").is_some() {
        for name in [
            "ae2f3f60-10c3-49ec-9131-0edba348a8b8",
            "pianoer03",
        ] {
            if let Some(part) = parsed.parts.get(name) {
                eprintln!(
                    "TRACE: part {:?} sample_path={:?} sample_offset={}",
                    name, part.sample_path, part.sample_offset
                );
                for ev in &part.events {
                    if ev.offset.is_some() || ev.sample_override.is_some() {
                        eprintln!(
                            "TRACE:   ev row={:.2} sample_override={:?} offset={:?}",
                            ev.row, ev.sample_override, ev.offset
                        );
                    }
                }
            }
        }
    }

    let title = ts_path
    .file_stem()
    .and_then(|s| s.to_str())
    .unwrap_or("(untitled)")
    .to_string();

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

// ── loader ─────────────────────────────────────────────────────────────

pub(crate) struct Loaded {
    pub(crate) samples: Vec<Scheduled>,
    pub(crate) vsts: Vec<VstNote>,
}

pub(crate) struct Loader {
    project: Arc<Project>,
    pub(crate) cache: Cache,
    pub(crate) cursor: usize,
}

impl Loader {
    pub(crate) fn new(project: Arc<Project>, cache: Cache) -> Self {
        Loader {
            project,
            cache,
            cursor: 0,
        }
    }

    pub(crate) fn load_due(&mut self, horizon: i64) -> Loaded {
        let project = Arc::clone(&self.project);
        let mut samples = Vec::new();
        let mut vsts = Vec::new();
        while self.cursor < project.order.len() {
            let li = project.order[self.cursor];
            if project.lines[li].min_start > horizon {
                break;
            }
            self.cursor += 1;
            let batch = prepare_line(&project, &mut self.cache, li, i64::MIN);
            samples.extend(batch.samples);
            vsts.extend(batch.vsts);
        }
        Loaded { samples, vsts }
    }

    pub(crate) fn seek(&mut self, to: i64, horizon: i64) -> Loaded {
        let project = Arc::clone(&self.project);
        self.cursor = 0;
        let mut samples = Vec::new();
        let mut vsts = Vec::new();
        while self.cursor < project.order.len() {
            let li = project.order[self.cursor];
            let line = &project.lines[li];
            if line.min_start > horizon {
                break;
            }
            self.cursor += 1;
            if line.max_end <= to {
                continue;
            }
            let batch = prepare_line(&project, &mut self.cache, li, to);
            samples.extend(batch.samples);
            vsts.extend(batch.vsts);
        }
        Loaded { samples, vsts }
    }
}

fn prepare_line(
    project: &Project,
    cache: &mut Cache,
    li: usize,
    min_end: i64,
) -> Loaded {
    let sr = project.sample_rate as f64;
    let placement_row = li as f64 * project.rows_per_measure;
    let tempo = &project.tempo;
    let debug = std::env::var_os("FLP_DEBUG").is_some();
    let mut samples = Vec::new();
    let mut vsts = Vec::new();
    let mut seq = 0u32;

    for name in &project.lines[li].parts {
        let part = &project.parts[name];
        let tpl = cache.template(
            project.part_hash[name],
            part,
            &project.path_to_id,
            &project.samples,
            &project.sf2_instruments,
            &project.vst_instruments,
            &project.multi_sample_instruments,
        );

        for tv in &tpl.voices {
            seq += 1;
            let start_row = placement_row + tv.rel_row;
            let end_row = start_row + tv.dur_rows;
            let start = (tempo.time_at_row(start_row) * sr) as i64;
            let end = (tempo.time_at_row(end_row).min(project.render_end_sec) * sr) as i64;
            if end <= start || end <= min_end {
                if debug {
                    eprintln!(
                        "flp: voice skipped: part={} sample_id={} start_row={} end_row={} start={} end={} min_end={} reason={}",
                        name,
                        tv.sample_id,
                        start_row,
                        end_row,
                        start,
                        end,
                        min_end,
                        if end <= start { "end<=start" } else { "end<=min_end" }
                    );
                }
                continue;
            }
            let mut sample = Arc::clone(&project.samples[&tv.sample_id]);
            if sample.len() == 0 {
                if debug {
                    eprintln!(
                        "flp: voice DEAD: part={} sample_id={} sample is empty",
                        name, tv.sample_id
                    );
                }
                continue;
            }

            if (tv.offset as f64) >= sample.len() as f64 {
                if debug {
                    eprintln!(
                        "flp: voice DEAD: part={} sample_id={} offset={} >= sample_len={} (whole voice silent)",
                        name, tv.sample_id, tv.offset, sample.len()
                    );
                }
            }

            let mut offset = tv.offset as f64;
            if let Some(src_bpm) = tv.src_bpm {
                let beat_secs = tempo.time_at_row(start_row + part.subs_per_beat as f64)
                - tempo.time_at_row(start_row);
                if beat_secs > 0.0 {
                    let song_bpm = 60.0 / beat_secs;
                    let factor = src_bpm / song_bpm;
                    if (factor - 1.0).abs() > 0.001 && factor > 0.05 && factor < 20.0 {
                        sample = cache.stretched(tv.sample_id, &sample, factor);
                        offset *= factor;
                    }
                }
            }
            let mk = |wp: &Vec<Waypoint>, initial: f64| -> Option<AutomationLane> {
                if wp.is_empty() {
                    return None;
                }
                let abs: Vec<Waypoint> = wp
                .iter()
                .map(|&(r, d, x)| (placement_row + r, d, x))
                .collect();
                Some(AutomationLane::new(initial, start_row, &abs, tempo))
            };
            samples.push(Scheduled {
                start,
                end,
                line: li as u32,
                seq,
                sample,
                pitch_hz: tv.pitch_hz,
                offset,
                vol: tv.vol,
                pan: tv.pan,
                pitch_lane: mk(&tv.pitch_wp, tv.pitch_hz),
                vol_lane: mk(&tv.vol_wp, tv.vol),
                pan_lane: mk(&tv.pan_wp, tv.pan),
                const_gain: gains(tv.vol, tv.pan, project.song_volume, project.linear_pan),
                part_path: part.sample_path.clone(),
                source_row: tv.rel_row,
            });
            if let Ok(target) = std::env::var("VOICE_TRACE_SAMPLE") {
                if let Ok(tid) = target.parse::<u32>() {
                    if tv.sample_id == tid {
                        eprintln!(
                            "vtrace: scheduled sample {} start={} end={} ({} samples, {:.3}s) offset={} pitch_hz={:.2} vol={}",
                            tv.sample_id,
                            start,
                            end,
                            end - start,
                            (end - start) as f64 / sr,
                            offset,
                            tv.pitch_hz,
                            tv.vol,
                        );
                    }
                }
            }
        }

        for vn in &tpl.vst_notes {
            seq += 1;
            let start_row = placement_row + vn.rel_row;
            let end_row = start_row + vn.dur_rows;
            let start = (tempo.time_at_row(start_row) * sr) as i64;
            let end = (tempo.time_at_row(end_row).min(project.render_end_sec) * sr) as i64;
            if end <= start || end <= min_end {
                continue;
            }
            vsts.push(VstNote {
                start,
                end,
                iid: vn.iid,
                note: vn.pitch_midi,
                velocity: vn.velocity,
                line: li as u32,
                seq,
                part_path: part.sample_path.clone(),
                source_row: vn.rel_row,
            });
        }
    }
    Loaded { samples, vsts }
}