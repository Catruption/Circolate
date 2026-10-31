//! Bridges portable patches to chip-specific writes.
//!
//! Two dispatch paths:
//!   - PSG (`tone.square`, `noise.lfsr`): honor `native.div`/`native.att`
//!     for pitch and start volume, then expand `amp` into scheduled `att`
//!     writes with multi-step ramps (no hardware envelope).
//!   - FM (`fm.operator`): write every field from `native.ym2612` at
//!     note-on time, then `key_on = 0xF0`, then `key_on = 0` at note-end.
//!     A 2 ms pre-off guarantees a clean off→on envelope retrigger,
//!     because YM2612 ignores key-on when the channel is already keyed on.
//!
//! Pitch for FM is derived from the note key at dispatch time (per-note
//! `fnum`/`block` writes) rather than baked into the patch. This matches
//! vgm2pre::compare(), which excludes fnum/block from instrument identity.

use std::collections::BTreeMap;

use crate::hosting::chip::chip::{Chip, CapValue, Role};
use crate::hosting::chip::patch::{Patch, Envelope};
use crate::hosting::chip::engine::runtime::ScheduledWrite;

const RLEPS: f64 = 1e-6;
const RAMP_STEPS: u32 = 12;
const RAMP_DUR_S: f64 = 0.015;
const STEAL_MUTE_S: f64 = 0.010;
/// Time between the forced key-off and the new note's key-on. Long enough
/// that ymfm's envelope state machine sees an off→on transition and
/// restarts attack; short enough to be imperceptible.
const FM_PRE_OFF_S: f64 = 0.002;

#[derive(Clone, Debug)]
pub struct Note {
    pub t: f64,
    pub dur: f64,
    pub key: u8,
    pub vel: u8,
    pub pitch_hz: Option<f64>,
    pub automation: Vec<AutomationPoint>,
    pub patch_name: String,
    pub voice_hint: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AutomationPoint {
    pub t: f64,
    pub ramp: f64,
    pub pitch_hz: Option<f64>,
    pub volume: Option<f64>,
}

pub enum Dispatch {
    Writes(Vec<ScheduledWrite>),
    Rendition { missing: Vec<String> },
}

pub struct Adapter {
    chip: Chip,
    slots: BTreeMap<String, VoiceSlot>,
}

struct VoiceSlot { _patch: String, release_t: f64 }

impl Adapter {
    pub fn new(chip: Chip) -> Self {
        Adapter { chip, slots: BTreeMap::new() }
    }

    fn off_value(&self) -> u32 {
        self.chip.provide("amp.attenuation")
        .and_then(|c| c.keys.get("off"))
        .and_then(|v| match v { CapValue::Raw(s) => s.parse().ok(), _ => None })
        .unwrap_or(15)
    }

    fn steps_value(&self) -> u32 {
        self.chip.provide("amp.attenuation")
        .and_then(|c| c.keys.get("steps"))
        .and_then(|v| match v { CapValue::Raw(s) => s.parse().ok(), _ => None })
        .unwrap_or(16)
    }

    fn volume_field_name(&self) -> Option<String> {
        self.chip.provide("amp.attenuation")
        .and_then(|c| c.keys.get("field"))
        .and_then(|v| match v { CapValue::Raw(s) => Some(s.clone()), _ => None })
        .or_else(|| {
            self.chip.fields.iter()
            .find(|(_, f)| f.role == Role::Volume)
            .map(|(n, _)| n.clone())
        })
    }

    fn pitch_field_name(&self) -> Option<String> {
        self.chip.provide("pitch.divider")
        .and_then(|c| c.keys.get("field"))
        .and_then(|v| match v { CapValue::Raw(s) => Some(s.clone()), _ => None })
        .or_else(|| self.chip.provide("pitch.fnum_block")
        .and_then(|c| c.keys.get("field"))
        .and_then(|v| match v { CapValue::Raw(s) => Some(s.clone()), _ => None }))
    }

    pub fn dispatch(&mut self, note: &Note, patch: &Patch) -> Dispatch {
        let mut missing = Vec::new();
        for req in &patch.requires {
            if self.chip.provide(&req.capability).is_none() {
                missing.push(req.capability.clone());
            }
        }
        if !missing.is_empty() {
            return Dispatch::Rendition { missing };
        }

        if patch.source.kind == "fm.operator" {
            return self.dispatch_fm(note, patch);
        }
        self.dispatch_psg(note, patch)
    }

    // ──────────────────────────────── FM path ────────────────────────────────

    fn dispatch_fm(&mut self, note: &Note, patch: &Patch) -> Dispatch {
        let cap = match self.chip.provide("fm.operator") {
            Some(c) => c,
            None => return Dispatch::Rendition { missing: vec!["fm.operator".into()] },
        };
        let sources = match cap.keys.get("sources") {
            Some(CapValue::List(l)) if !l.is_empty() => l.clone(),
            _ => return Dispatch::Rendition { missing: vec!["fm.operator sources".into()] },
        };

        let (voice, _stolen) = self.allocate(
            &sources, &patch.name, note.t + note.dur, note.voice_hint.as_deref(),
        );

        let native = match patch.native.get(&self.chip.name) {
            Some(n) => n,
            None => return Dispatch::Rendition {
                missing: vec![format!("native {}", self.chip.name)],
            },
        };

        let mut writes = Vec::new();

        // Forced key-off 2ms before the new note. YM2612 ignores key-on
        // when a channel is already keyed on — the envelope does not
        // retrigger from registers, it continues from its current level.
        writes.push(ScheduledWrite {
            t: (note.t - FM_PRE_OFF_S).max(0.0),
                    voice: voice.clone(),
                    field: "key_on".into(),
                    value: 0x00,
        });

        for (field, value) in native {
            // Pitch is derived from the note key (below), not the patch body.
            // Legacy patches with baked fnum/block/key_scale must not override.
            if matches!(field.as_str(),
                "key_on" | "key" | "fnum" | "block" | "key_scale") { continue; }
                writes.push(ScheduledWrite {
                    t: note.t,
                    voice: voice.clone(),
                            field: field.clone(),
                            value: *value as u32,
                });
        }

        writes.extend(self.fm_pitch_writes(&voice, note, patch, note.t));
        let mut current_hz = note.pitch_hz.unwrap_or_else(|| {
            let key_f = note.key as f64 + patch.pitch.transpose as f64 + patch.pitch.detune as f64 / 100.0;
            440.0 * 2f64.powf((key_f - 69.0) / 12.0)
        });
        for point in &note.automation {
            if let Some(target_hz) = point.pitch_hz {
                writes.extend(self.fm_pitch_ramp_writes(&voice, target_hz, current_hz, point.t, point.ramp));
                current_hz = target_hz;
            }
        }

        writes.push(ScheduledWrite {
            t: note.t,
            voice: voice.clone(),
                    field: "key_on".into(),
                    value: 0xF0,
        });

        writes.push(ScheduledWrite {
            t: note.t + note.dur,
            voice: voice.clone(),
                    field: "key_on".into(),
                    value: 0x00,
        });

        Dispatch::Writes(writes)
    }

    /// YM2612 pitch from note key. Inverse of
    /// `freq = fnum * (clock/144) / 2^(21-block)`:
    /// `fnum = freq * 144 * 2^(21-block) / clock`. Pick the lowest block
    /// that yields fnum in [1, 2047] — that gives the highest in-range fnum
    /// and therefore the best pitch resolution. If freq is out of range,
    /// clamp to block 7 (too high) or block 0 (too low).
    fn fm_pitch_writes(
        &self,
        voice: &str,
        note: &Note,
        patch: &Patch,
        t: f64,
    ) -> Vec<ScheduledWrite> {
        let clock = self.chip.clock as f64;
        if clock <= 0.0 { return Vec::new(); }
        let key_f = note.key as f64
        + patch.pitch.transpose as f64
        + patch.pitch.detune as f64 / 100.0;
        let freq = note.pitch_hz.unwrap_or_else(|| 440.0 * 2f64.powf((key_f - 69.0) / 12.0));
        self.fm_pitch_frequency_writes(voice, freq, t)
    }

    fn fm_pitch_ramp_writes(
        &self, voice: &str, target_hz: f64, from_hz: f64, t: f64, ramp: f64,
    ) -> Vec<ScheduledWrite> {
        if ramp <= 0.0 { return self.fm_pitch_frequency_writes(voice, target_hz, t); }
        let from = from_hz.max(1.0).log2();
        let to = target_hz.max(1.0).log2();
        let mut out = Vec::new();
        for i in 1..=RAMP_STEPS {
            let f = i as f64 / RAMP_STEPS as f64;
            out.extend(self.fm_pitch_frequency_writes(voice, 2f64.powf(from + (to - from) * f), t + ramp * f));
        }
        out
    }

    fn fm_pitch_frequency_writes(&self, voice: &str, freq: f64, t: f64) -> Vec<ScheduledWrite> {
        if !freq.is_finite() || freq <= 0.0 { return Vec::new(); }
        let clock = self.chip.clock as f64;
        if clock <= 0.0 { return Vec::new(); }

        let mut block = 7u8;
        let mut fnum = 2047u32;
        let mut found = false;
        for b in 0..=7u8 {
            let f = freq * 144.0 * 2f64.powi(21 - b as i32) / clock;
            let fn_val = f.round() as u32;
            if fn_val >= 1 && fn_val <= 2047 {
                block = b;
                fnum = fn_val;
                found = true;
                break;
            }
        }
        if !found {
            let f_at_b0 = freq * 144.0 * 2f64.powi(21) / clock;
            if f_at_b0 > 2047.0 {
                // Too high: use block 7 (lowest fnum) at fnum 2047.
                block = 7; fnum = 2047;
            } else {
                // Too low: use block 0 (highest fnum) at fnum 1.
                block = 0; fnum = 1;
            }
        }

        vec![
            ScheduledWrite { t, voice: voice.into(), field: "fnum".into(),  value: fnum },
            ScheduledWrite { t, voice: voice.into(), field: "block".into(), value: block as u32 },
        ]
    }

    // ──────────────────────────────── PSG path ───────────────────────────────

    fn dispatch_psg(&mut self, note: &Note, patch: &Patch) -> Dispatch {
        let cap = match self.chip.provide(&patch.source.kind) {
            Some(c) => c,
            None => return Dispatch::Rendition { missing: vec![patch.source.kind.clone()] },
        };
        let sources = match cap.keys.get("sources") {
            Some(CapValue::List(l)) if !l.is_empty() => l.clone(),
            _ => return Dispatch::Rendition {
                missing: vec![format!("{} (no sources)", patch.source.kind)],
            },
        };
        let (voice, stolen) = self.allocate(
            &sources, &patch.name, note.t + note.dur, note.voice_hint.as_deref(),
        );

        let mut writes = Vec::new();

        if stolen {
            if let Some(f) = self.volume_field_name() {
                writes.push(ScheduledWrite {
                    t: (note.t - STEAL_MUTE_S).max(0.0),
                            voice: voice.clone(),
                            field: f,
                            value: self.off_value(),
                });
            }
        }

        if let Some(w) = self.pitch_write(&voice, patch, note) {
            writes.push(w);
        }
        writes.extend(self.envelope_writes(&voice, patch, note));
        let pitch_field = self.pitch_field_name();
        let pitch_range = self.chip.provide("pitch.divider")
            .and_then(|c| c.keys.get("range"))
            .and_then(|v| match v { CapValue::Raw(s) => parse_range(s), _ => None })
            .unwrap_or((1, 1023));
        let volume_field = self.volume_field_name();
        let max_att = self.steps_value().saturating_sub(1);
        let mut current_att = (((127u32.saturating_sub(note.vel as u32)) as f64 / 127.0) * max_att as f64).round();
        let mut current_hz = note.pitch_hz.unwrap_or_else(|| {
            let key_f = note.key as f64 + patch.pitch.transpose as f64 + patch.pitch.detune as f64 / 100.0;
            440.0 * 2f64.powf((key_f - 69.0) / 12.0)
        });
        for point in &note.automation {
            if let (Some(target_hz), Some(field)) = (point.pitch_hz, pitch_field.as_ref()) {
                let from = current_hz.max(1.0).log2();
                let to = target_hz.max(1.0).log2();
                for i in 1..=RAMP_STEPS {
                    let f = i as f64 / RAMP_STEPS as f64;
                    let hz = 2f64.powf(from + (to - from) * f);
                    let div = (self.chip.clock as f64 / (32.0 * hz)).round() as u32;
                    writes.push(ScheduledWrite { t: point.t + point.ramp * f, voice: voice.clone(), field: field.clone(), value: div.clamp(pitch_range.0, pitch_range.1) });
                }
                current_hz = target_hz;
            }
            if let (Some(volume), Some(field)) = (point.volume, volume_field.as_ref()) {
                let to_att = (((100.0 - volume.clamp(0.0, 100.0)) / 100.0) * max_att as f64).round();
                let steps = if point.ramp > 0.0 { RAMP_STEPS } else { 1 };
                for i in 1..=steps {
                    let f = i as f64 / steps as f64;
                    let att = (current_att * (1.0 - f) + to_att * f).round() as u32;
                    writes.push(ScheduledWrite { t: point.t + point.ramp * f, voice: voice.clone(), field: field.clone(), value: att });
                }
                current_att = to_att;
            }
        }
        writes.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        Dispatch::Writes(writes)
    }

    fn allocate(
        &mut self,
        sources: &[String],
        patch: &str,
        release_t: f64,
        hint: Option<&str>,
    ) -> (String, bool) {
        if let Some(h) = hint {
            if sources.iter().any(|s| s == h) {
                let was_active = self.slots.get(h)
                .map(|s| s.release_t > release_t - 0.5)
                .unwrap_or(false);
                self.slots.insert(h.into(), VoiceSlot { _patch: patch.into(), release_t });
                return (h.into(), was_active);
            }
        }
        for s in sources {
            if !self.slots.contains_key(s) {
                self.slots.insert(s.clone(), VoiceSlot { _patch: patch.into(), release_t });
                return (s.clone(), false);
            }
        }
        let oldest = self.slots.iter()
        .min_by(|a, b| a.1.release_t.partial_cmp(&b.1.release_t).unwrap())
        .map(|(k, _)| k.clone())
        .unwrap_or_else(|| sources[0].clone());
        let was_active = self.slots.get(&oldest)
        .map(|s| s.release_t > release_t - 0.5)
        .unwrap_or(false);
        self.slots.insert(oldest.clone(), VoiceSlot { _patch: patch.into(), release_t });
        (oldest, was_active)
    }

    fn pitch_write(&self, voice: &str, patch: &Patch, note: &Note) -> Option<ScheduledWrite> {
        if let Some(native) = patch.native.get(&self.chip.name) {
            if let Some(&div) = native.get("div") {
                return Some(ScheduledWrite {
                    t: note.t, voice: voice.into(),
                            field: self.pitch_field_name().unwrap_or_else(|| "div".into()),
                            value: div as u32,
                });
            }
        }

        if let Some(cap) = self.chip.provide("pitch.divider") {
            let field = match cap.keys.get("field") {
                Some(CapValue::Raw(s)) => s.clone(),
                _ => return None,
            };
            let range = cap.keys.get("range")
            .and_then(|v| match v { CapValue::Raw(s) => parse_range(s), _ => None })
            .unwrap_or((1, 1023));
            let key_f = note.key as f64 + patch.pitch.transpose as f64 + patch.pitch.detune as f64 / 100.0;
            let f = note.pitch_hz.unwrap_or_else(|| 440.0 * 2f64.powf((key_f - 69.0) / 12.0));
            let n = (self.chip.clock as f64 / (32.0 * f)).round() as u32;
            let n = n.clamp(range.0, range.1);
            return Some(ScheduledWrite { t: note.t, voice: voice.into(), field, value: n });
        }
        None
    }

    fn envelope_writes(&self, voice: &str, patch: &Patch, note: &Note) -> Vec<ScheduledWrite> {
        let field = match self.volume_field_name() { Some(f) => f, None => return Vec::new() };
        let off = self.off_value();
        let steps = self.steps_value();
        let max_att = steps.saturating_sub(1);
        let end_t = note.t + note.dur;
        let mut writes: Vec<ScheduledWrite> = Vec::new();

        let vel_peak = ((127u32.saturating_sub(note.vel as u32)) * max_att) / 127;

        let ramp = |from: u32, to: u32, start_t: f64, out: &mut Vec<ScheduledWrite>| {
            let (a, b) = (from.min(max_att), to.min(max_att));
            if a == b { return; }
            for i in 1..=RAMP_STEPS {
                let frac = i as f64 / RAMP_STEPS as f64;
                let att = (a as f64 * (1.0 - frac) + b as f64 * frac).round() as u32;
                out.push(ScheduledWrite {
                    t: start_t + RAMP_DUR_S * frac,
                    voice: voice.into(),
                         field: field.clone(),
                         value: att,
                });
            }
        };

        match &patch.amp {
            Envelope::None => {
                let start_att = patch.native.get(&self.chip.name)
                .and_then(|n| n.get("att"))
                .map(|v| *v as u32)
                .unwrap_or(vel_peak)
                .min(max_att);

                writes.push(ScheduledWrite {
                    t: note.t, voice: voice.into(), field: field.clone(), value: off,
                });
                ramp(off, start_att, note.t, &mut writes);

                let release_start = (end_t - RAMP_DUR_S).max(note.t + RAMP_DUR_S * 2.0);
                ramp(start_att, off, release_start, &mut writes);
            }
            Envelope::Adsr { a, d, s, r: _, curve: _ } => {
                let base_att = patch.native.get(&self.chip.name)
                .and_then(|n| n.get("att"))
                .map(|v| *v as u32)
                .unwrap_or(0)
                .min(max_att);
                let peak_att = (base_att + vel_peak).min(max_att);
                let sustain_att = ((peak_att as f32
                + (off as f32 - peak_att as f32) * (1.0 - s.clamp(0.0, 1.0)))
                .round() as u32).min(off);

                let max_attack = (note.dur * 0.25).max(RAMP_DUR_S);
                let attack_dur = (*a as f64).max(RAMP_DUR_S).min(max_attack);
                let attack_steps = ((attack_dur / RAMP_DUR_S) as u32).max(RAMP_STEPS);
                for i in 0..=attack_steps {
                    let frac = i as f64 / attack_steps as f64;
                    let att = (off as f64 * (1.0 - frac) + peak_att as f64 * frac).round() as u32;
                    writes.push(ScheduledWrite {
                        t: note.t + attack_dur * frac,
                        voice: voice.into(), field: field.clone(), value: att,
                    });
                }

                let decay_start = note.t + attack_dur;
                let decay_end = (decay_start + (*d as f64).max(RAMP_DUR_S)).min(end_t);
                if decay_end > decay_start && sustain_att != peak_att {
                    let decay_steps = RAMP_STEPS.max(((decay_end - decay_start) / RAMP_DUR_S) as u32);
                    for i in 1..=decay_steps {
                        let frac = i as f64 / decay_steps as f64;
                        let att = (peak_att as f64 * (1.0 - frac) + sustain_att as f64 * frac)
                        .round() as u32;
                        writes.push(ScheduledWrite {
                            t: decay_start + (decay_end - decay_start) * frac,
                                    voice: voice.into(), field: field.clone(), value: att,
                        });
                    }
                }

                let release_start = (end_t - RAMP_DUR_S).max(decay_end);
                if end_t - release_start >= RAMP_DUR_S / 2.0 {
                    ramp(sustain_att, off, release_start, &mut writes);
                } else {
                    writes.push(ScheduledWrite {
                        t: end_t, voice: voice.into(), field: field.clone(), value: off,
                    });
                }
            }
            Envelope::Steps { values, loop_at, rate } => {
                let step_dur = 1.0 / (*rate).max(1.0) as f64;
                for (i, &v) in values.iter().enumerate() {
                    let at = note.t + (i as f64) * step_dur;
                    if at >= end_t { break; }
                    writes.push(ScheduledWrite {
                        t: at, voice: voice.into(), field: field.clone(),
                                value: (v as u32).min(off),
                    });
                }
                if let Some(loop_start) = loop_at {
                    let mut t = note.t + values.len() as f64 * step_dur;
                    while t < end_t - RLEPS {
                        for &v in &values[*loop_start..] {
                            if t >= end_t - RLEPS { break; }
                            writes.push(ScheduledWrite {
                                t, voice: voice.into(), field: field.clone(),
                                        value: (v as u32).min(off),
                            });
                            t += step_dur;
                        }
                    }
                }
                let last = writes.last().map(|w| w.value).unwrap_or(off);
                ramp(last, off, (end_t - RAMP_DUR_S).max(note.t), &mut writes);
            }
        }

        writes.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        writes
    }
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once("..")?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}
