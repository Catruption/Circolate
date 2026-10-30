//! Streaming loader: emits voices for a horizon of upcoming blocks.

use std::sync::{Arc, Mutex};

use std::sync::OnceLock;

use crate::timing::{AutomationLane, TempoMap};

use super::cache::Cache;
use super::engine::{gains, Scheduled, VstNote};
use super::project::Project;

pub(crate) struct Loaded {
    pub(crate) samples: Vec<Scheduled>,
    pub(crate) vsts: Vec<VstNote>,
}

pub(crate) struct Loader {
    project: Arc<Project>,
    pub(crate) cache: Arc<Mutex<Cache>>,
    pub(crate) cursor: usize,
}

impl Loader {
    /// Construct from an owned cache, wrapping it in `Arc<Mutex<_>>`.
    /// Used by the worker thread when no pre-warm has been done.
    #[allow(dead_code)]
    pub(crate) fn new(project: Arc<Project>, cache: Cache) -> Self {
        Loader {
            project,
            cache: Arc::new(Mutex::new(cache)),
            cursor: 0,
        }
    }

    /// Construct from a shared cache, so pre-warm results persist.
    pub(crate) fn with_shared_cache(project: Arc<Project>, cache: Arc<Mutex<Cache>>) -> Self {
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
        let mut cache = self.cache.lock().unwrap();
        while self.cursor < project.order.len() {
            let li = project.order[self.cursor];
            if project.lines[li].min_start > horizon {
                break;
            }
            self.cursor += 1;
            let batch = prepare_line(&project, &mut cache, li, i64::MIN);
            samples.extend(batch.samples);
            vsts.extend(batch.vsts);
        }
        samples.sort_unstable_by_key(Scheduled::key);
        vsts.sort_unstable_by_key(VstNote::key);
        Loaded { samples, vsts }
    }

    /// Build every template and time-stretch the song will need, without
    /// materializing any voices.  Replaces `load_due(i64::MAX)` as the
    /// startup pre-warm.
    pub(crate) fn warm(&mut self) {
        let project = Arc::clone(&self.project);
        let mut cache = self.cache.lock().unwrap();
        for li in 0..project.lines.len() {
            let line_base_row = project.line_start_row(li);
            for call in project.line_calls(li) {
                let Some(part) = project.parts.get(&call.name) else { continue };
                let tpl = cache.template(
                    project.part_hash_map()[&call.name],
                                         part,
                                         &project.path_to_id,
                                         &project.samples,
                                         &project.sf2_instruments,
                                         &project.sf2_engine_specs,
                                         &project.vst_instruments,
                                         &project.multi_sample_instruments,
                );
                let ms = call.pattern_measure_start;
                let me = ms + call.pattern_measure_len;
                let call_base_row = line_base_row + call.line_offset;
                for tv in &tpl.voices {
                    let Some(src_bpm) = tv.src_bpm else { continue };
                    if tv.rel_row < ms || tv.rel_row >= me {
                        continue;
                    }
                    let start_row = call_base_row + (tv.rel_row - ms);
                    let Some(f) =
                    stretch_factor(&project.tempo, part.subs_per_beat, start_row, src_bpm)
                    else {
                        continue;
                    };
                    if let Some(s) = project.samples.get(&tv.sample_id) {
                        if s.len() > 0 {
                            cache.stretched(tv.sample_id, s, f);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn seek(&mut self, to: i64, horizon: i64) -> Loaded {
        let project = Arc::clone(&self.project);
        self.cursor = 0;
        let mut samples = Vec::new();
        let mut vsts = Vec::new();
        let mut cache = self.cache.lock().unwrap();
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
            let batch = prepare_line(&project, &mut cache, li, to);
            samples.extend(batch.samples);
            vsts.extend(batch.vsts);
        }
        samples.sort_unstable_by_key(Scheduled::key);
        vsts.sort_unstable_by_key(VstNote::key);
        Loaded { samples, vsts }
    }
}

fn trace_sample() -> Option<u32> {
    static T: OnceLock<Option<u32>> = OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("VOICE_TRACE_SAMPLE")
        .ok()
        .and_then(|s| s.parse().ok())
    })
}

fn flp_debug() -> bool {
    static D: OnceLock<bool> = OnceLock::new();
    *D.get_or_init(|| std::env::var_os("FLP_DEBUG").is_some())
}

/// Time-stretch factor for a voice whose source tempo differs from the
/// song's at `start_row`, or `None` if it plays as-is.  Quantized to
/// 0.001 so a tempo ramp doesn't mint a fresh stretch for every voice.
fn stretch_factor(tempo: &TempoMap, subs_per_beat: u32, start_row: f64, src_bpm: f64) -> Option<f64> {
    let beat_secs =
    tempo.time_at_row(start_row + subs_per_beat as f64) - tempo.time_at_row(start_row);
    if beat_secs <= 0.0 {
        return None;
    }
    let song_bpm = 60.0 / beat_secs;
    let factor = ((src_bpm / song_bpm) * 1000.0).round() / 1000.0;
    ((factor - 1.0).abs() > 0.001 && factor > 0.01 && factor < 100.0).then_some(factor)
}

pub(crate) fn prepare_line(
    project: &Project,
    cache: &mut Cache,
    li: usize,
    min_end: i64,
) -> Loaded {
    let sr = project.sample_rate as f64;
    let line_base_row = project.line_start_row(li);
    let tempo = &project.tempo;
    let debug = flp_debug();
    let mut samples = Vec::new();
    let mut vsts = Vec::new();
    let mut seq = 0u32;

    for call in project.line_calls(li) {
        let Some(part) = project.parts.get(&call.name) else { continue };
        let tpl = cache.template(
            project.part_hash_map()[&call.name],
                                 part,
                                 &project.path_to_id,
                                 &project.samples,
                                 &project.sf2_instruments,
                                 &project.sf2_engine_specs,
                                 &project.vst_instruments,
                                 &project.multi_sample_instruments,
        );

        let measure_start = call.pattern_measure_start;
        let measure_end = call.pattern_measure_start + call.pattern_measure_len;
        let call_base_row = line_base_row + call.line_offset;

        for tv in &tpl.voices {
            if tv.rel_row < measure_start || tv.rel_row >= measure_end {
                continue;
            }
            let local = tv.rel_row - measure_start;
            seq += 1;

            let start_row = call_base_row + local;
            let end_row = start_row + tv.dur_rows;
            let start = (tempo.time_at_row(start_row) * sr) as i64;
            let end = (tempo.time_at_row(end_row).min(project.render_end_sec) * sr) as i64;
            if end <= start || end <= min_end {
                if debug {
                    eprintln!(
                        "flp: voice skipped: part={} sample_id={} start_row={} end_row={} start={} end={} min_end={} reason={}",
                        call.name, tv.sample_id, start_row, end_row, start, end, min_end,
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
                        call.name, tv.sample_id
                    );
                }
                continue;
            }

            let (mut vol, mut pan) = (tv.vol, tv.pan);
            if let Some(ov) = &call.overrides {
                if let Some(v) = ov.vol { vol = v; }
                if let Some(p) = ov.pan { pan = p; }
            }

            let mut offset = tv.offset as f64;
            if let Some(src_bpm) = tv.src_bpm {
                if let Some(factor) =
                    stretch_factor(tempo, part.subs_per_beat, start_row, src_bpm)
                    {
                        sample = cache.stretched(tv.sample_id, &sample, factor);
                        offset *= factor;
                    }
            }

            let mk = |wp: &Vec<_>, initial: f64| -> Option<AutomationLane> {
                if wp.is_empty() { return None; }
                let abs: Vec<_> = wp
                .iter()
                .map(|&(r, d, x)| (call_base_row + r - measure_start, d, x))
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
                vol,
                pan,
                pitch_lane: mk(&tv.pitch_wp, tv.pitch_hz),
                         vol_lane: mk(&tv.vol_wp, vol),
                         pan_lane: mk(&tv.pan_wp, pan),
                         const_gain: gains(vol, pan, project.song_volume, project.linear_pan),
                         part_path: part.sample_path.clone(),
                         source_row: tv.rel_row,
                         env: tv.env,
            });

            if let Some(target) = trace_sample() {
                if tv.sample_id == target {
                    eprintln!(
                        "vtrace: scheduled sample {} start={} end={} ({} samples, {:.3}s) offset={} pitch_hz={:.2} vol={}",
                              tv.sample_id, start, end, end - start,
                              (end - start) as f64 / sr, offset, tv.pitch_hz, vol
                    );
                }
            }
        }

        if !tpl.vst_notes.is_empty() {
            // One shared path per part call; every note clones the Arc.
            let vst_path: Arc<str> = Arc::from(part.sample_path.as_str());
            for vn in &tpl.vst_notes {
                if vn.rel_row < measure_start || vn.rel_row >= measure_end {
                    continue;
                }
                let local = vn.rel_row - measure_start;
                seq += 1;
                let start_row = call_base_row + local;
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
                    part_path: Arc::clone(&vst_path),
                          source_row: vn.rel_row,
                });
            }
        }
    }
    Loaded { samples, vsts }
}
