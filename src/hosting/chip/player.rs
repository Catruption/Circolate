//! One `ChipPlayer` per chip model. Holds every patch that targets the
//! chip; `note_on` looks up the patch by `part_path`. Two backends:
//! `CoreState` (primitive DSL, for PSG) and `YmfmBackend` (ymfm-sys, for
//! YM2612).
//!
//! Channel pinning: each patch name carries its source channel
//! (`FM1_xxx` → `ch1`, `PSG2_xxx` → `tone2`). `note_on` computes the
//! hint, cancels any pending writes to that voice from previous notes,
//! and passes it to the adapter. This prevents cross-channel voice churn
//! (which manifests as clicks or silence on everything but the lead).

use std::collections::HashMap;
use std::sync::Arc;

use crate::hosting::chip::chip::{
    Chip, CapValue, Capability, Field, Role, Scope, Voice,
};
use crate::hosting::chip::patch::Patch;
use crate::hosting::chip::adapter::{Adapter, Dispatch, Note};
use crate::hosting::chip::engine::core::ChipCore;
use crate::hosting::chip::engine::runtime::CoreState;
use crate::hosting::chip::dac::{DacState, DacTrack};

/// `CIRCOLATE_CHIP_DEBUG=1` turns on per-note dispatch logging. It used to be
/// unconditional `eprintln!` on the audio thread, which can glitch playback.
fn chip_debug() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CIRCOLATE_CHIP_DEBUG").is_some())
}

/// How far ahead of the engine clock queued notes are handed to the adapter.
/// Notes are dispatched in start-time order per voice, never in arrival order.
const DISPATCH_LOOKAHEAD_S: f64 = 0.3;

#[derive(Clone, Debug)]
pub struct ChipNote {
    pub start: i64,
    pub end: i64,
    pub iid: u16,
    pub note: u8,
    pub velocity: u8,
    pub pitch_hz: Option<f64>,
    pub automation: Vec<ChipAutomation>,
    pub line: u32,
    pub seq: u32,
    pub part_path: Arc<str>,
    pub source_row: f64,
}

#[derive(Clone, Debug)]
pub struct ChipAutomation {
    pub at: i64,
    pub ramp_samples: i64,
    pub pitch_hz: Option<f64>,
    pub volume: Option<f64>,
}

pub struct ChipPlayer {
    engine: Box<dyn ChipCore>,
    adapter: Adapter,
    bank: HashMap<String, Patch>,
    host_sr: f64,
    ratio: f64,
    phase: f64,
    prev: f32,
    dc_prev_in: f32,
    dc_prev_out: f32,
    active: Vec<ChipNote>,
    off_value: u32,
    volume_field: String,
    /// VGM DAC events routed through YM2612 channel 6.
    dac: Option<DacState>,
    /// Notes waiting to be dispatched (see `flush_due`).
    queue: Vec<ChipNote>,
    /// Engine clock in host samples: advanced by `render`, reset by `seek`.
    now: i64,
    /// Start sample of the latest note dispatched per voice (late-arrival guard).
    last_start: HashMap<String, i64>,
    /// Chip mix and isolated per-voice waveform taps for tracker views.
    scope_slot: Option<usize>,
    scope_samples: Vec<f32>,
    voice_scope: Vec<(usize, Vec<f32>)>,
    voice_acc: Vec<f32>,
    voice_prev: Vec<f32>,
    dac_scope: Vec<f32>,
}

impl ChipPlayer {
    pub fn new(chip: Chip, host_sr: u32) -> Result<Self, String> {
        let engine: Box<dyn ChipCore> = Box::new(CoreState::new(chip.clone())?);
        let adapter = Adapter::new(chip.clone());

        let volume_field = chip.fields.iter()
            .find(|(_, f)| f.role == Role::Volume)
            .map(|(name, _)| name.clone())
            .ok_or("chip has no field with role=volume")?;

        let off_value = chip.provide("amp.attenuation")
            .and_then(|c| c.keys.get("off"))
            .and_then(|v| match v {
                CapValue::Raw(s) => s.parse::<u32>().ok(),
                _ => None,
            })
            .unwrap_or(15);

        let ratio = engine.step_rate() / host_sr as f64;
        let scope_slot = match chip.name.as_str() {
            "sn76489" => Some(crate::player::scope::VGM_SLOT_PSG),
            "ym2612" => Some(crate::player::scope::VGM_SLOT_YM),
            _ => None,
        };
        let voice_scope: Vec<_> = engine.voices().iter()
            .filter_map(|v| crate::player::scope::vgm_voice_scope_slot(v).map(|s| (s, Vec::new())))
            .collect();
        let voice_count = voice_scope.len();

        Ok(Self {
            engine, adapter,
            bank: HashMap::new(),
            host_sr: host_sr as f64,
            ratio, phase: 0.0, prev: 0.0,
            dc_prev_in: 0.0, dc_prev_out: 0.0,
            active: Vec::new(),
            off_value, volume_field,
            dac: None,
            queue: Vec::new(),
            now: 0,
            last_start: HashMap::new(),
            scope_slot,
            scope_samples: Vec::new(),
            voice_acc: vec![0.0; voice_scope.len()], voice_scope,
            voice_prev: vec![0.0; voice_count], dac_scope: Vec::new(),
        })
    }

    pub fn new_ymfm(clock: u32, host_sr: u32) -> Result<Self, String> {
        use crate::hosting::chip::ymfm::YmfmBackend;

        let chip = ym2612_chip_shell(clock);
        let backend = YmfmBackend::new(clock, host_sr)?;
        let ratio = backend.step_rate() / host_sr as f64;
        let voice_scope: Vec<_> = backend.voices().iter()
            .filter_map(|v| crate::player::scope::vgm_voice_scope_slot(&v).map(|s| (s, Vec::new())))
            .collect();
        let adapter = Adapter::new(chip);

        Ok(Self {
            engine: Box::new(backend),
            adapter,
            bank: HashMap::new(),
            host_sr: host_sr as f64,
            ratio, phase: 0.0, prev: 0.0,
            dc_prev_in: 0.0, dc_prev_out: 0.0,
            active: Vec::new(),
            // seek()/panic() silence a voice by writing volume_field = off_value.
            // On FM that means key-off (key_on = 0), not a TL write (a bare
            // "tl" has no operator digit and was silently ignored).
            off_value: 0,
            volume_field: "key_on".into(),
            dac: None,
            queue: Vec::new(),
            now: 0,
            last_start: HashMap::new(),
            scope_slot: Some(crate::player::scope::VGM_SLOT_YM),
            scope_samples: Vec::new(),
            voice_acc: vec![0.0; voice_scope.len()], voice_scope,
            voice_prev: vec![0.0; 6], dac_scope: Vec::new(),
        })
    }

    /// Give this player the PCM data and hit list for DAC notes
    /// (see `DacTrack::extract_dac_track` / `extract_dac_track(vgm_bytes)`).
    pub fn set_dac(&mut self, track: DacTrack) {
        if self.engine.dac_enabled() || self.scope_slot == Some(crate::player::scope::VGM_SLOT_YM) {
            let dac = DacState::new(track, self.host_sr as u32);
            self.engine.reserve_register_writes(dac.max_event_count());
            self.dac = Some(dac);
        }
    }

    pub fn add_patch(&mut self, path: String, patch: Patch) {
        self.bank.insert(path, patch);
    }

    pub fn patch_count(&self) -> usize { self.bank.len() }

    pub fn note_on(&mut self, note: &ChipNote) {
        // DAC parts are sample playback, not a synthesised voice: route them
        // to DacState by part name, before (and regardless of) the patch
        // lookup, since the DAC patch needs `sample.pcm` which no chip has.
        let path: &str = note.part_path.as_ref();
        if path.rsplit('/').next().map_or(false, |f| f.starts_with("DAC_")) {
            if let Some(d) = self.dac.as_mut() {
                d.schedule_note(note.start, self.engine.as_mut());
            } else if chip_debug() {
                eprintln!("CHIP dac note but no DacTrack set: {}", path);
            }
            return;
        }
        if !self.bank.contains_key(note.part_path.as_ref()) {
            if chip_debug() {
                eprintln!("CHIP no patch loaded for part {}", path);
            }
            return;
        }
        // Don't dispatch yet. A channel is split into one part per pitch, so
        // notes arrive grouped by part, not by time. Dispatching in arrival
        // order made each note's `cancel_pending(voice, t)` delete the
        // already-scheduled LATER notes of the same channel (missing notes).
        // `flush_due` dispatches them in start-time order instead.
        self.active.push(note.clone());
        self.queue.push(note.clone());
    }

    /// Dispatch every queued note starting before `upto`, in time order.
    fn flush_due(&mut self, upto: i64) {
        if self.queue.is_empty() { return; }
        self.queue.sort_by(|a, b| {
            a.start.cmp(&b.start).then(a.line.cmp(&b.line)).then(a.seq.cmp(&b.seq))
        });
        let n = self.queue.partition_point(|n| n.start < upto);
        if n == 0 { return; }
        let due: Vec<ChipNote> = self.queue.drain(..n).collect();
        for note in &due {
            self.dispatch_note(note);
        }
    }

    fn dispatch_note(&mut self, note: &ChipNote) {
        let patch = match self.bank.get(note.part_path.as_ref()) {
            Some(p) => p,
            None => return,
        };
        let t = note.start as f64 / self.host_sr;
        let mut dur = ((note.end - note.start).max(1)) as f64 / self.host_sr;
        let voice_hint = channel_hint_from_patch_name(&patch.name);

        if let Some(ref v) = voice_hint {
            let last = self.last_start.get(v).copied();
            match last {
                // Arrived after a LATER note on this voice was dispatched
                // (only possible if the sequencer is late). Don't cancel
                // that note's writes; clip this one so it can't run into it.
                Some(ls) if note.start < ls => {
                    let limit = ls as f64 / self.host_sr - 0.003;
                    if chip_debug() {
                        eprintln!("CHIP late note: patch={} t={:.3} voice={} (later note already at {:.3})",
                                  patch.name, t, v, ls as f64 / self.host_sr);
                    }
                    dur = dur.min(limit - t);
                    if dur < 0.002 { return; }
                }
                _ => {
                    // Reusing the voice: cancel the previous note's pending
                    // key-off / att-off so they don't land mid-new-note.
                    self.engine.cancel_pending(v, t);
                    self.last_start.insert(v.clone(), note.start);
                }
            }
        }

        let internal = Note {
            t, dur,
            key: note.note,
            vel: note.velocity,
            pitch_hz: note.pitch_hz,
            automation: note.automation.iter().map(|p| crate::hosting::chip::adapter::AutomationPoint {
                t: p.at as f64 / self.host_sr,
                ramp: p.ramp_samples.max(0) as f64 / self.host_sr,
                pitch_hz: p.pitch_hz,
                volume: p.volume,
            }).collect(),
            patch_name: patch.name.clone(),
            voice_hint,
        };
        match self.adapter.dispatch(&internal, patch) {
            Dispatch::Writes(ws) => {
                if chip_debug() {
                    eprintln!("CHIP dispatch: patch={} t={:.3} dur={:.4} writes={} key={}",
                              patch.name, t, dur, ws.len(), note.note);
                }
                for w in ws { self.engine.schedule(w); }
            }
            Dispatch::Rendition { missing } => {
                if chip_debug() {
                    eprintln!("CHIP rendition (note dropped): patch={} missing={:?}", patch.name, missing);
                }
            }
        }
    }

    pub fn note_off(&mut self, note: &ChipNote) {
        self.active.retain(|n| !(n.line == note.line && n.seq == note.seq));
    }

    pub fn seek(&mut self, to_sample: i64) {
        let voices = self.engine.voices();
        let field = self.volume_field.clone();
        let off = self.off_value;
        for v in voices {
            self.engine.set_field(&v, &field, off);
        }
        self.engine.clear_pending();
        self.engine.set_time(to_sample as f64 / self.host_sr);
        self.active.clear();
        self.queue.clear();
        self.last_start.clear();
        self.now = to_sample;
        self.phase = 0.0;
        self.prev = 0.0;
        self.dc_prev_in = 0.0;
        self.dc_prev_out = 0.0;
    }

    pub fn panic(&mut self) {
        self.active.clear();
        self.queue.clear();
        self.last_start.clear();
        let voices = self.engine.voices();
        let field = self.volume_field.clone();
        let off = self.off_value;
        for v in voices {
            self.engine.set_field(&v, &field, off);
        }
        self.engine.clear_pending();
    }

    pub fn render(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        _block_start: i64,
        gain: f32,
    ) {
        self.scope_samples.clear();
        self.dac_scope.clear();
        self.scope_samples.reserve(out_l.len());
        for (slot, samples) in &mut self.voice_scope {
            let _ = slot;
            samples.clear();
            samples.reserve(out_l.len());
        }
        let upto = self.now
            + (DISPATCH_LOOKAHEAD_S * self.host_sr) as i64
            + out_l.len() as i64;
        self.flush_due(upto);
        self.now += out_l.len() as i64;

        for i in 0..out_l.len() {
            self.phase += self.ratio;
            // Box-filter every core step that lands in this host sample
            // instead of point-sampling (removes most aliasing from the
            // 223 kHz PSG squares and the ~1.2:1 FM ratio).
            let mut acc = 0.0f32;
            let mut n = 0u32;
            self.voice_acc.fill(0.0);
            while self.phase >= 1.0 {
                self.phase -= 1.0;
                acc += self.engine.step();
                for (voice_idx, (_, sample)) in self.engine.voice_samples().iter().enumerate() {
                    if let Some(acc) = self.voice_acc.get_mut(voice_idx) {
                        *acc += *sample;
                    }
                }
                n += 1;
            }
            if n > 0 { self.prev = acc / n as f32; }
            let s = self.prev;
            let out_dc = s - self.dc_prev_in + 0.9995 * self.dc_prev_out;
            self.dc_prev_in = s;
            self.dc_prev_out = out_dc;
            let g = out_dc * gain;
            out_l[i] += g;
            out_r[i] += g;
            self.scope_samples.push(g);
            if n > 0 {
                for (voice_idx, (_, samples)) in self.voice_scope.iter_mut().enumerate() {
                    let dac_voice = voice_idx == 5 && self.engine.dac_enabled();
                    self.voice_prev[voice_idx] = if dac_voice { 0.0 } else { self.voice_acc[voice_idx] / n as f32 };
                    samples.push(self.voice_prev[voice_idx] * gain);
                }
                let dac_sample = if self.engine.dac_enabled() && self.voice_acc.len() > 5 {
                    self.voice_acc[5] / n as f32 * gain
                } else { 0.0 };
                self.dac_scope.push(dac_sample);
            } else {
                for (voice_idx, (_, samples)) in self.voice_scope.iter_mut().enumerate() {
                    samples.push(self.voice_prev[voice_idx] * gain);
                }
                self.dac_scope.push(0.0);
            }
        }
        if let Some(slot) = self.scope_slot {
            crate::player::scope::scope_push_block(
                slot, &self.scope_samples, &self.scope_samples, 1.0,
            );
        }
        for (slot, samples) in &self.voice_scope {
            crate::player::scope::scope_push_block(*slot, samples, samples, 1.0);
        }
        if self.dac.is_some() {
            crate::player::scope::scope_push_block(
                crate::player::scope::VGM_SLOT_DAC, &self.dac_scope, &self.dac_scope, 1.0,
            );
        }
    }
}

/// Extract the source-channel from a patch name. `FM1_xxx` → `ch1`,
/// `PSG2_xxx` → `tone2`, `NOISE_xxx` → `noise`, `DAC_xxx` → `dac`.
fn channel_hint_from_patch_name(name: &str) -> Option<String> {
    let prefix = name.split('_').next()?;
    match prefix {
        "FM1" => Some("ch1".into()),
        "FM2" => Some("ch2".into()),
        "FM3" => Some("ch3".into()),
        "FM4" => Some("ch4".into()),
        "FM5" => Some("ch5".into()),
        "FM6" => Some("ch6".into()),
        "PSG1" => Some("tone1".into()),
        "PSG2" => Some("tone2".into()),
        "PSG3" => Some("tone3".into()),
        "NOISE" => Some("noise".into()),
        "DAC" => Some("dac".into()),
        _ => None,
    }
}

/// Minimal `Chip` shell for the adapter. `ym2612.chip` currently fails to
/// parse (placeholder table bodies), so we construct the pieces the
/// adapter needs (`provides`, `voices`, `name`, `clock`) manually.
fn ym2612_chip_shell(clock: u32) -> Chip {
    let mut chip = Chip::default();
    chip.name = "ym2612".into();
    chip.clock = clock as u64;
    chip.voices = (1..=6).map(|n| Voice {
        name: format!("ch{}", n),
        kind: "fm".into(),
    }).collect();

    let mut fm_cap = Capability::default();
    fm_cap.keys.insert("sources".into(), CapValue::List(vec![
        "ch1".into(), "ch2".into(), "ch3".into(),
        "ch4".into(), "ch5".into(), "ch6".into(),
    ]));
    chip.provides.insert("fm.operator".into(), fm_cap);

    let mut pitch_cap = Capability::default();
    pitch_cap.keys.insert("field".into(), CapValue::Raw("fnum".into()));
    chip.provides.insert("pitch.fnum_block".into(), pitch_cap);

    let mut env_cap = Capability::default();
    env_cap.keys.insert("field".into(), CapValue::Raw("tl".into()));
    chip.provides.insert("amp.envelope".into(), env_cap);

    let mut pan_cap = Capability::default();
    pan_cap.keys.insert("field".into(), CapValue::Raw("lr".into()));
    chip.provides.insert("pan.lr".into(), pan_cap);

    chip.fields.insert("tl".into(), Field {
        name: "tl".into(),
        parts: vec![],
        role: Role::Volume,
        scope: Scope::Op,
        enum_hint: None,
    });

    chip
}
