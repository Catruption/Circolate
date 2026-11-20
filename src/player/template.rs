//! Part templates: a compiled form of a halotext part, ready for
//! placement into the arrangement.

use std::collections::HashMap;
use std::sync::Arc;

use crate::formats::halotext::parse::{HalotextPart, C5_HZ};
use crate::timing::Waypoint;

use super::types::{
    MultiSampleInstrument, Sample, Sf2EngineSpec, Sf2Instrument, Sf2VoiceEnvelope,
    VstInstrument,
};

pub(crate) struct TVoice {
    pub(crate) rel_row: f64,
    pub(crate) dur_rows: f64,
    pub(crate) pitch_hz: f64,
    pub(crate) vol: f64,
    pub(crate) pan: f64,
    pub(crate) sample_id: u32,
    pub(crate) offset: u32,
    pub(crate) src_bpm: Option<f64>,
    pub(crate) pitch_wp: Vec<Waypoint>,
    pub(crate) vol_wp: Vec<Waypoint>,
    pub(crate) pan_wp: Vec<Waypoint>,
    pub(crate) env: Option<Sf2VoiceEnvelope>,
}

#[derive(Clone)]
pub(crate) struct VstNoteProto {
    pub(crate) rel_row: f64,
    pub(crate) dur_rows: f64,
    pub(crate) pitch_midi: u8,
    pub(crate) velocity: u8,
    pub(crate) iid: u16,
    pub(crate) fine_pitch_cents: f64,
}

pub(crate) struct PartTemplate {
    pub(crate) voices: Vec<TVoice>,
    pub(crate) vst_notes: Vec<VstNoteProto>,
}

/// Build the composite SF2 lookup key.  Bank 0 is written as `path#preset`;
/// nonzero bank as `path#bank:preset`.  Non-SF2 paths pass through unchanged.
fn sf2_key(path: &str, preset: Option<i16>, bank: Option<i16>) -> String {
    if !path.ends_with(".sf2") {
        return path.to_string();
    }
    let p = preset.unwrap_or(0);
    let b = bank.unwrap_or(0);
    if b == 0 {
        format!("{}#{}", path, p)
    } else {
        format!("{}#{}:{}", path, b, p)
    }
}

pub(crate) fn build_template(
    part: &HalotextPart,
    path_to_id: &HashMap<String, u32>,
    samples: &HashMap<u32, Arc<Sample>>,
    sf2_instruments: &HashMap<String, Arc<Sf2Instrument>>,
    sf2_engine_specs: &HashMap<String, Sf2EngineSpec>,
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

            // Composite SF2 key.  Priority: per-event preset override >
            // part-level preset > legacy path-as-key.
            let sf2_key_str: String = if let Some(_) = ev.preset {
                sf2_key(lookup_path, ev.preset, ev.bank)
            } else if part.sample_path.ends_with(".sf2") {
                sf2_key(&part.sample_path, part.preset, part.bank)
            } else {
                lookup_path.to_string()
            };

            let mut sid = default_sid;
            if let Some(p) = &ev.sample_override {
                sid = path_to_id.get(p).copied().or(default_sid);
            }

            let dur_beats = ev.dur_beats.filter(|d| *d != 0.0).unwrap_or(1.0);

            let fp_cents = ev.fine_pitch.unwrap_or(0) as f64;
            let fp_ratio = 2f64.powf(fp_cents / 1200.0);

            // Path 0: multi-sample instrument.
            if let Some(inst) = multi_sample_instruments.get(lookup_path) {
                let key = ev.pitch_midi.unwrap_or(60).clamp(0, 127) as u8;
                if let Some((sid, pitch_hz)) = inst.resolve(key, fp_cents) {
                    if samples.contains_key(&sid) {
                        voices.push(TVoice {
                            rel_row: ev.row,
                            dur_rows: dur_beats * subs,
                            pitch_hz,
                            vol: ev.vol.or(part.default_vol).unwrap_or(100.0),
                            pan: ev.pan.unwrap_or(0.0),
                            sample_id: sid,
                            offset: ev.offset.unwrap_or(part.sample_offset),
                            src_bpm: None,
                            pitch_wp: Vec::new(),
                            vol_wp: Vec::new(),
                            pan_wp: Vec::new(),
                            env: None,
                        });
                        last = Some(voices.len() - 1);
                        continue;
                    }
                }
            }

            // Path 1: direct single WAV sample.
            if let Some(sid) = sid {
                if samples.contains_key(&sid) {
                    let base_hz = ev.pitch_hz.unwrap_or(C5_HZ);
                    voices.push(TVoice {
                        rel_row: ev.row,
                        dur_rows: dur_beats * subs,
                        pitch_hz: base_hz * fp_ratio,
                        vol: ev.vol.or(part.default_vol).unwrap_or(100.0),
                        pan: ev.pan.unwrap_or(0.0),
                        sample_id: sid,
                        offset: ev.offset.unwrap_or(part.sample_offset),
                        src_bpm: ev.src_bpm.or(part.src_bpm),
                        pitch_wp: Vec::new(),
                        vol_wp: Vec::new(),
                        pan_wp: Vec::new(),
                        env: None,
                    });
                    last = Some(voices.len() - 1);
                    continue;
                }
            }

            // Path 2: VST instrument sidecar.
            if let Some(inst) = vst_instruments.get(lookup_path) {
                let pitch = ev.pitch_midi.unwrap_or(60).clamp(0, 127) as u8;
                let vol_pct = ev.vol.or(part.default_vol).unwrap_or(100.0).clamp(0.0, 100.0);
                let vel = (vol_pct * 127.0 / 100.0).round().clamp(1.0, 127.0) as u8;
                vst_notes.push(VstNoteProto {
                    rel_row: ev.row,
                    dur_rows: dur_beats * subs,
                    pitch_midi: pitch,
                    velocity: vel,
                    iid: inst.iid,
                    fine_pitch_cents: fp_cents,
                });
                continue;
            }

            // Path 3: SF2 engine spec — composite key.
            if let Some(spec) = sf2_engine_specs.get(&sf2_key_str) {
                let pitch = ev.pitch_midi.unwrap_or(60).clamp(0, 127) as u8;
                let vol_pct = ev.vol.or(part.default_vol).unwrap_or(100.0).clamp(0.0, 100.0);
                let vel = (vol_pct * 127.0 / 100.0).round().clamp(1.0, 127.0) as u8;
                vst_notes.push(VstNoteProto {
                    rel_row: ev.row,
                    dur_rows: dur_beats * subs,
                    pitch_midi: pitch,
                    velocity: vel,
                    iid: spec.iid,
                    fine_pitch_cents: fp_cents,
                });
                continue;
            }

            // Fallback: internal renderer — composite key.
            if let Some(inst) = sf2_instruments.get(&sf2_key_str) {
                let key = ev.pitch_midi.unwrap_or(60).clamp(0, 127) as u8;
                let vel_pct = ev.vol.or(part.default_vol).unwrap_or(100.0).clamp(0.0, 100.0);
                let vel = (vel_pct * 127.0 / 100.0).round().clamp(0.0, 127.0) as u8;
                if let Some(spec) = inst.resolve(key, vel, fp_cents) {
                    if samples.contains_key(&spec.sample_id) {
                        voices.push(TVoice {
                            rel_row: ev.row,
                            dur_rows: dur_beats * subs,
                            pitch_hz: spec.pitch_hz,
                            vol: ev.vol.or(part.default_vol).unwrap_or(100.0),
                            pan: ev.pan.unwrap_or(0.0),
                            sample_id: spec.sample_id,
                            offset: ev.offset.unwrap_or(part.sample_offset),
                            src_bpm: None,
                            pitch_wp: Vec::new(),
                            vol_wp: Vec::new(),
                            pan_wp: Vec::new(),
                            env: Some(spec.env),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::halotext::model::HalotextEvent;

    fn part(default_vol: Option<f64>) -> HalotextPart {
        HalotextPart {
            name: "P".into(),
            sample_path: "P.wav".into(),
            beats_per_measure: 4,
            subs_per_beat: 4,
            len_beats: None,
            sample_offset: 0,
            preset: None,
            bank: None,
            src_bpm: None,
            events: vec![HalotextEvent { row: 0.0, ..Default::default() }],
            measures: Vec::new(),
            section_lengths: Vec::new(),
            default_vol,
            default_pan: None,
            default_dur: None,
            shims: Vec::new(),
            slicex: None,
        }
    }

    fn build(p: &HalotextPart) -> PartTemplate {
        let sample = Arc::new(Sample {
            left: vec![0.0; 4],
            right: vec![0.0; 4],
            stereo: false,
            rate: 44_100.0,
            loop_pts: None,
        });
        build_template(
            p,
            &HashMap::from([(p.sample_path.clone(), 1)]),
            &HashMap::from([(1, sample)]),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
    }

    #[test]
    fn part_default_volume_applies_when_note_omits_it() {
        let p = part(Some(50.0));
        let tpl = build(&p);
        assert_eq!(tpl.voices.len(), 1);
        assert_eq!(tpl.voices[0].vol, 50.0);
    }

    #[test]
    fn note_volume_overrides_part_default() {
        let mut p = part(Some(50.0));
        p.events[0].vol = Some(80.0);
        let tpl = build(&p);
        assert_eq!(tpl.voices[0].vol, 80.0);
    }
}
