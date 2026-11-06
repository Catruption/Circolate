//! Slicex → inline Halotext parts.

use crate::formats::halotext::model::{
    HalotextEvent, HalotextPart, Slicex, SlicexEnvRef, SlicexSlice,
};
use super::trest::EnvelopePoint;

pub(super) struct HalotextBuild {
    pub parts: Vec<HalotextPart>,
}

pub(super) fn build(
    inst_name: &str,
    wav_rel: &str,
    ranges: &[(usize, usize)],
                    keys: &[u32],
                    pan: u32,
                    atten: u32,
                    cutoff: u32,
                    amp_env:   Option<&[EnvelopePoint]>,
                    speed_env: Option<&[EnvelopePoint]>,
                    cut_env:   Option<&[EnvelopePoint]>,
                    lfo_env:   Option<&[EnvelopePoint]>,
                    env_scale_beats: f64,
) -> HalotextBuild {
    let mut parts: Vec<HalotextPart> = Vec::new();
    let mut slicex = Slicex::default();

    for (&key, &(start, end)) in keys.iter().zip(ranges.iter()) {
        slicex.slices.push(SlicexSlice {
            frame_start: start as u32,
            frame_end:   end   as u32,
            key, pan, atten, cutoff,
        });
    }

    if let Some(pts) = amp_env {
        let name = format!("{inst_name}_amp");
        parts.push(build_env_part(&name, pts, env_scale_beats));
        slicex.amp = Some(SlicexEnvRef { part: name, amount: 1.0 });
    }
    if let Some(pts) = speed_env {
        let name = format!("{inst_name}_speed");
        parts.push(build_env_part(&name, pts, env_scale_beats));
        slicex.speed = Some(SlicexEnvRef { part: name, amount: 0.5 });
    }
    if let Some(pts) = cut_env {
        let name = format!("{inst_name}_cut");
        parts.push(build_env_part(&name, pts, env_scale_beats));
        slicex.cut = Some(SlicexEnvRef { part: name, amount: 0.75 });
    }
    if let Some(pts) = lfo_env {
        let name = format!("{inst_name}_lfo");
        parts.push(build_env_part(&name, pts, env_scale_beats));
        slicex.lfo = Some(SlicexEnvRef { part: name, amount: 0.0 });
    }

    parts.push(HalotextPart {
        name: inst_name.to_string(),
               sample_path: wav_rel.to_string(),
               beats_per_measure: 4,
               subs_per_beat: 4,
               len_beats: None,
               sample_offset: 0, preset: None, bank: None, src_bpm: None,
               events: Vec::new(), measures: Vec::new(), section_lengths: Vec::new(),
               default_vol: None, default_pan: None, default_dur: None,
                   shims: Vec::new(),
               slicex: Some(slicex),
    });
    HalotextBuild { parts }
}

fn build_env_part(name: &str, pts: &[EnvelopePoint], scale_beats: f64) -> HalotextPart {
    let mut events: Vec<HalotextEvent> = Vec::with_capacity(pts.len());
    let mut row = 0.0f64;
    for (i, p) in pts.iter().enumerate() {
        row += p.dx;
        let row_beats = row * scale_beats;
        let kind = if i == 0 { 0u8 } else { p.kind() };
        let tension = if i == 0 { 0.0 } else { p.curve as f64 };
        events.push(HalotextEvent {
            op: "env".to_string(), row: row_beats,
                    source_line: 0, source_byte_column: 0, source_end_byte_column: 0,
                    pitch_midi: None, pitch_hz: None, vol: None, pan: None, dur_beats: None,
                    sample_override: None, offset: None, bank: None, preset: None,
                    root_note: None, src_bpm: None, fine_pitch: None, release: None,
                    mod_x: None, mod_y: None, lyric: None,
                    raw: String::new(), waypoints: Vec::new(),
                    env_value: Some(p.y), env_tension: Some(tension), env_kind: Some(kind),
        });
    }
    HalotextPart {
        name: name.to_string(), sample_path: String::new(),
        beats_per_measure: 4, subs_per_beat: 4, len_beats: None, sample_offset: 0,
        preset: None, bank: None, src_bpm: None,
        events: events.clone(),
        measures: vec![events], section_lengths: vec![16.0],
        default_vol: None, default_pan: None, default_dur: None,
            shims: Vec::new(), slicex: None,
    }
}
