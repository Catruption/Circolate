//! Voice scheduling, mixer, and DSP primitives.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use crate::effects::EffectChain;
use crate::hosting::sf2::engine::Sf2Engine;
use crate::formats::trackscript::C5_HZ;
use crate::timing::AutomationLane;
use crate::mixer::mute;
use crate::hosting::vst::ffi::VstHost;

use super::project::{Project, Sample, Sf2VoiceEnvelope};
use super::inserts;

mod dsp;
pub(crate) use dsp::gains;
use dsp::{env_level_at, interpolate, mix_add, sanitize, up_table, EnvState};

// ── scheduled sample voice ─────────────────────────────────────────────

pub(crate) struct Scheduled {
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) line: u32,
    pub(crate) seq: u32,
    pub(crate) sample: Arc<Sample>,
    pub(crate) pitch_hz: f64,
    pub(crate) offset: f64,
    pub(crate) vol: f64,
    pub(crate) pan: f64,
    pub(crate) pitch_lane: Option<AutomationLane>,
    pub(crate) vol_lane: Option<AutomationLane>,
    pub(crate) pan_lane: Option<AutomationLane>,
    pub(crate) const_gain: (f64, f64),
    pub(crate) part_path: String,
    pub(crate) source_row: f64,
    pub(crate) env: Option<Sf2VoiceEnvelope>,
}

impl Scheduled {
    pub(crate) fn key(&self) -> (i64, u32, u32) {
        (self.start, self.line, self.seq)
    }
}

// ── scheduled VST note ─────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub(crate) struct VstNote {
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) iid: u16,
    pub(crate) note: u8,
    pub(crate) velocity: u8,
    pub(crate) line: u32,
    pub(crate) seq: u32,
    pub(crate) part_path: String,
    pub(crate) source_row: f64,
}

impl VstNote {
    pub(crate) fn key(&self) -> (i64, u32, u32) {
        (self.start, self.line, self.seq)
    }
}

// ── engine ─────────────────────────────────────────────────────────────

struct Active {
    v: Scheduled,
    raw_pos: f64,
    env_state: Option<EnvState>,
}

pub(crate) struct ShimHost {
    pub(crate) host: VstHost,
    pub(crate) mix: f64,
    pub(crate) gain: f64,
    pub(crate) pan: f64,
}

pub(crate) enum Msg {
    Batch { epoch: u64, samples: Vec<Scheduled>, vsts: Vec<VstNote> },
    Seek  { epoch: u64, pos: i64, samples: Vec<Scheduled>, vsts: Vec<VstNote> },
}

const VST_BLOCK: i64 = 512;

const VST_HISTORY_SECS: f64 = 10.0;

const HOLD_TAIL_SECS: f64 = 0.020;

pub(crate) struct Engine {
    sr: f64,
    song_volume: f64,
    linear: bool,
    total: i64,
    pub(crate) pos: i64,
    pub(crate) ready: bool,
    epoch: u64,
    pending: VecDeque<Scheduled>,
    active: Vec<Active>,

    insert_chains: BTreeMap<u8, inserts::InsertChain>,
    channel_to_insert: BTreeMap<u16, u8>,
    insert_scratch: BTreeMap<u8, (Vec<f32>, Vec<f32>)>,
    sf2_scratch: BTreeMap<u16, (Vec<f32>, Vec<f32>)>,

    hosts: BTreeMap<u16, VstHost>,
    host_scratch: BTreeMap<u16, (Vec<f32>, Vec<f32>)>,
    vst_gains: BTreeMap<u16, f32>,
    iid_to_part_path: BTreeMap<u16, String>,

    sf2_engines: BTreeMap<u16, Sf2Engine>,

    vst_pending: VecDeque<VstNote>,
    vst_active: Vec<VstNote>,
    vst_log: Vec<VstNote>,
    vst_held: BTreeMap<u16, u128>,
    vst_synced: bool,
    vst_scratch_l: Vec<f32>,
    vst_scratch_r: Vec<f32>,

    vst_hist_l: Vec<f32>,
    vst_hist_r: Vec<f32>,
    vst_hist_head: i64,

    master_fx: Option<EffectChain>,

    // Pause Tail state
    pause_fade: f32,
    tail_frames: i64,

    // ── shim subsystem ───────────────────────────────────────────
    shims: BTreeMap<u32, ShimHost>,
    shim_by_line: BTreeMap<u32, Vec<u32>>,
    shim_scratch: BTreeMap<u32, (Vec<f32>, Vec<f32>)>,
    shim_dry_l: Vec<f32>,
    shim_dry_r: Vec<f32>,
    shim_chains_cache: Vec<Vec<u32>>,
    shim_processed: Vec<u32>,
    shim_link_l: Vec<f32>,
    shim_link_r: Vec<f32>,

    vst_events_buf: Vec<(i32, u8, u16, u8, u8)>,
}

impl Engine {
    pub(crate) fn new(p: &Project) -> Self {
        up_table(); // build the sinc table now, not mid-callback
        let hist_len = (VST_HISTORY_SECS * p.sample_rate as f64).max(1.0) as usize;
        Engine {
            sr: p.sample_rate as f64,
            song_volume: p.song_volume,
            linear: p.linear_pan,
            total: p.total_samples,
            pos: 0,
            ready: false,
            epoch: 0,
            pending: VecDeque::new(),
            active: Vec::new(),
            hosts: BTreeMap::new(),
            host_scratch: BTreeMap::new(),
            vst_gains: BTreeMap::new(),
            iid_to_part_path: BTreeMap::new(),
            sf2_engines: BTreeMap::new(),
            vst_pending: VecDeque::new(),
            vst_active: Vec::new(),
            vst_log: Vec::new(),
            vst_held: BTreeMap::new(),
            vst_synced: true,
            vst_scratch_l: vec![0.0; VST_BLOCK as usize],
            vst_scratch_r: vec![0.0; VST_BLOCK as usize],
            vst_hist_l: vec![0.0; hist_len],
            vst_hist_r: vec![0.0; hist_len],
            vst_hist_head: i64::MIN,
            insert_chains: BTreeMap::new(),
            channel_to_insert: BTreeMap::new(),
            insert_scratch: BTreeMap::new(),
            sf2_scratch: BTreeMap::new(),
            master_fx: None,
            pause_fade: 1.0,
            tail_frames: 0,
            shims: BTreeMap::new(),
            shim_by_line: BTreeMap::new(),
            shim_scratch: BTreeMap::new(),
            shim_dry_l: Vec::new(),
            shim_dry_r: Vec::new(),
            shim_chains_cache: Vec::new(),
            shim_processed: Vec::with_capacity(64),
            shim_link_l: vec![0.0; VST_BLOCK as usize],
            shim_link_r: vec![0.0; VST_BLOCK as usize],
            vst_events_buf: Vec::with_capacity(128),
        }
    }

    pub(crate) fn install_insert_chain(&mut self, insert: u8, chain: inserts::InsertChain) {
        self.insert_chains.insert(insert, chain);
        self.insert_scratch
        .entry(insert)
        .or_insert_with(|| (vec![0.0; VST_BLOCK as usize], vec![0.0; VST_BLOCK as usize]));
    }

    pub(crate) fn set_channel_insert(&mut self, iid: u16, insert: u8) {
        self.channel_to_insert.insert(iid, insert);
    }

    pub(crate) fn install_host(&mut self, iid: u16, host: VstHost) {
        self.install_host_with_gain(iid, host, 1.0);
    }

    pub(crate) fn install_host_with_gain(&mut self, iid: u16, host: VstHost, gain: f32) {
        self.hosts.insert(iid, host);
        self.host_scratch.insert(
            iid,
            (
                vec![0.0f32; VST_BLOCK as usize],
             vec![0.0f32; VST_BLOCK as usize],
            ),
        );
        self.vst_gains.insert(iid, gain);
        self.vst_held.entry(iid).or_insert(0);
    }

    pub(crate) fn register_part_path(&mut self, iid: u16, part_path: String) {
        self.iid_to_part_path.insert(iid, part_path);
    }

    pub(crate) fn install_sf2_engine(&mut self, iid: u16, engine: Sf2Engine) {
        self.sf2_engines.insert(iid, engine);
        self.sf2_scratch.insert(iid, (vec![0.0; VST_BLOCK as usize], vec![0.0; VST_BLOCK as usize]));
        self.vst_held.entry(iid).or_insert(0);
    }

    pub(crate) fn host_count(&self) -> usize {
        self.hosts.len()
    }

    pub(crate) fn sf2_engine_count(&self) -> usize {
        self.sf2_engines.len()
    }

    pub(crate) fn install_master_fx(&mut self, chain: EffectChain) {
        self.master_fx = Some(chain);
    }

    pub(crate) fn master_fx_count(&self) -> usize {
        self.master_fx.as_ref().map_or(0, |c| c.len())
    }

    pub(crate) fn sample_rate(&self) -> f64 { self.sr }

    pub(crate) fn install_shim(
        &mut self,
        id: u32,
        host: VstHost,
        mix: f64,
        gain: f64,
        pan: f64,
    ) {
        self.shims.insert(id, ShimHost {
            host,
            mix: mix.clamp(0.0, 1.0),
                          gain,
                          pan: pan.clamp(-1.0, 1.0),
        });
        self.shim_scratch.insert(
            id,
            (vec![0.0f32; VST_BLOCK as usize], vec![0.0f32; VST_BLOCK as usize]),
        );
    }

    pub(crate) fn attach_shim_line(&mut self, line: u32, shim_id: u32) {
        let chain = self.shim_by_line.entry(line).or_default();
        if !chain.contains(&shim_id) {
            chain.push(shim_id);
        }
        let mut all: Vec<Vec<u32>> = self.shim_by_line.values().cloned().collect();
        all.sort();
        all.dedup();
        self.shim_chains_cache = all;
    }

    pub(crate) fn shim_count(&self) -> usize {
        self.shims.len()
    }

    fn raw_pos_at(&self, v: &Scheduled, to: i64) -> f64 {
        if to <= v.start {
            return v.offset;
        }
        match &v.pitch_lane {
            None => {
                v.offset
                + (to - v.start) as f64
                * (v.sample.rate * (v.pitch_hz / C5_HZ) / self.sr)
            }
            Some(lane) => {
                let mut p = v.offset;
                for f in v.start..to {
                    p += v.sample.rate
                    * (lane.value_at(f as f64 / self.sr) / C5_HZ)
                    / self.sr;
                }
                p
            }
        }
    }

    fn env_state_for(&self, v: &Scheduled) -> Option<EnvState> {
        let env = v.env?;
        let sr = self.sr;
        let note_dur_sec = ((v.end - v.start) as f64 / sr).max(0.0);

        let delay_sec = env.delay_sec.max(0.0) as f64;
        let attack_end = env.attack_sec as f64;
        let hold_end = attack_end + env.hold_sec as f64;
        let decay_end = hold_end + env.decay_sec as f64;

        let release_sec = (env.release_sec as f64).max(0.020);
        let release_start = note_dur_sec;
        let release_end = note_dur_sec + release_sec;

        let attack_inv = if env.attack_sec > 0.0 { 1.0 / env.attack_sec } else { 0.0 };
        let decay_inv = if env.decay_sec > 0.0 { 1.0 / env.decay_sec } else { 0.0 };
        let release_inv = (1.0 / release_sec) as f32;

        let peak = env.peak.clamp(0.0, 1.0);
        let sustain = peak * env.sustain_fraction.clamp(0.0, 1.0);

        Some(EnvState {
            delay_sec,
            attack_end,
            hold_end,
            decay_end,
            release_start,
            release_end,
            peak,
            sustain,
            attack_inv,
            decay_inv,
            release_inv,
            start_sample: v.start,
        })
    }

    pub(crate) fn ingest(&mut self, mut voices: Vec<Scheduled>) {
        voices.sort_unstable_by_key(Scheduled::key);

        if self.pending.capacity() < self.pending.len() + voices.len() + 64 {
            self.pending.reserve(voices.len() + 64);
        }
        if self.active.capacity() < self.active.len() + voices.len() + 64 {
            self.active.reserve(voices.len() + 64);
        }

        for v in voices {
            if v.end <= self.pos {
                continue;
            }
            if v.start < self.pos {
                if !mute::is_note_active(&v.part_path, v.line as usize, v.source_row as i64) {
                    continue;
                }
                let raw_pos = self.raw_pos_at(&v, self.pos);
                let env_state = self.env_state_for(&v);
                self.active.push(Active { v, raw_pos, env_state });
            } else {
                let key = v.key();
                let idx = self
                .pending
                .binary_search_by_key(&key, |p| p.key())
                .unwrap_or_else(|i| i);
                self.pending.insert(idx, v);
            }
        }
    }

    pub(crate) fn ingest_vst(&mut self, mut notes: Vec<VstNote>) {
        notes.sort_unstable_by_key(VstNote::key);

        if self.vst_pending.capacity() < self.vst_pending.len() + notes.len() + 64 {
            self.vst_pending.reserve(notes.len() + 64);
        }
        if self.vst_log.capacity() < self.vst_log.len() + notes.len() + 64 {
            self.vst_log.reserve(notes.len() + 64);
        }

        const VST_LOG_MAX: usize = 10_000;
        if self.vst_log.len() + notes.len() > VST_LOG_MAX {
            let drop = ((self.vst_log.len() + notes.len()) - VST_LOG_MAX)
            .min(self.vst_log.len());
            self.vst_log.drain(..drop);
        }

        for mut n in notes {
            self.vst_log.push(n.clone());
            if n.end <= self.pos {
                continue;
            }
            if n.start < self.pos {
                n.start = self.pos;
            }
            let key = n.key();
            let idx = self
            .vst_pending
            .binary_search_by_key(&key, |p| p.key())
            .unwrap_or_else(|i| i);
            self.vst_pending.insert(idx, n);
        }
    }

    pub(crate) fn apply_seek(&mut self, to: i64, voices: Vec<Scheduled>) {
        self.pos = to.clamp(0, self.total);
        self.active.clear();
        self.pending.clear();
        self.panic_hosts();
        self.vst_pending.clear();
        self.vst_log.clear();
        self.vst_synced = true;
        self.vst_hist_head = i64::MIN;
        self.ready = true;
        self.ingest(voices);
    }

    fn panic_hosts(&mut self) {
        for (iid, mask) in self.vst_held.iter_mut() {
            if let Some(host) = self.hosts.get_mut(iid) {
                for k in 0..128u8 {
                    if (*mask >> k) & 1 == 1 {
                        host.note_off_at(k, 0);
                    }
                }
            }
            *mask = 0;
        }
        for eng in self.sf2_engines.values_mut() {
            eng.all_notes_off();
        }
        self.vst_active.clear();
    }

    fn resync_vsts(&mut self, pos: i64) {
        self.panic_hosts();
        let mut rebuilt: Vec<VstNote> = Vec::new();
        for n in &self.vst_log {
            if n.end <= pos {
                continue;
            }
            let mut n = n.clone();
            if n.start < pos {
                n.start = pos;
            }
            rebuilt.push(n);
        }
        rebuilt.sort_by_key(VstNote::key);
        self.vst_pending = rebuilt.into();
        self.vst_synced = true;
    }

    pub(crate) fn handle(&mut self, m: Msg) {
        match m {
            Msg::Batch { epoch, samples, vsts } => {
                if epoch == self.epoch {
                    self.ingest(samples);
                    self.ingest_vst(vsts);
                }
            }
            Msg::Seek { epoch, pos, samples, vsts } => {
                self.epoch = epoch;
                self.apply_seek(pos, samples);
                self.ingest_vst(vsts);
            }
        }
    }

    pub(crate) fn finished(&self) -> bool {
        self.ready && self.pos >= self.total
    }

    pub(crate) fn active_count(&self) -> usize {
        self.active.len()
    }

    pub(crate) fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32], speed: i64) {
        out_l.fill(0.0);
        out_r.fill(0.0);

        if !self.shims.is_empty() {
            let need = out_l.len();
            for buf in self.shim_scratch.values_mut() {
                if buf.0.len() < need {
                    buf.0.resize(need, 0.0);
                    buf.1.resize(need, 0.0);
                }
                buf.0[..need].fill(0.0);
                buf.1[..need].fill(0.0);
            }
        }

        // Pause and tail logic
        if speed != 0 {
            self.pause_fade = 1.0;
            self.tail_frames = (self.sr * 1.0) as i64; // 1 second of tail
        } else if self.tail_frames <= 0 {
            return; // Fully faded, do nothing
        }

        let block_len = out_l.len() as i64;
        let block_start = self.pos;
        // If paused, block_end is the same as block_start (sequencer doesn't advance)
        let block_end = if speed != 0 { block_start + block_len * speed } else { block_start };

        let sr = self.sr;
        let dir: i64 = if speed > 0 { 1 } else if speed < 0 { -1 } else { 0 };
        let dir_f = dir as f64;

        if speed != 0 {
            if speed > 0 {
                while self.pending.front().map_or(false, |v| v.start < block_end) {
                    let v = self.pending.pop_front().unwrap();
                    if !mute::is_note_active(&v.part_path, v.line as usize, v.source_row as i64) {
                        continue;
                    }
                    let raw_pos = v.offset;
                    let env_state = self.env_state_for(&v);
                    self.active.push(Active { v, raw_pos, env_state });
                }
            } else {
                let mut i = 0;
                while i < self.pending.len() {
                    let activate = {
                        let v = &self.pending[i];
                        v.end <= block_start && v.end > block_end
                    };
                    if activate {
                        let v = self.pending.remove(i).unwrap();
                        if !mute::is_note_active(&v.part_path, v.line as usize, v.source_row as i64) {
                            continue;
                        }
                        let step = v.sample.rate * (v.pitch_hz / C5_HZ) / sr;
                        let note_len = (v.end - v.start).max(0) as f64;
                        let raw_pos = v.offset + (note_len - 1.0).max(0.0) * step;
                        self.active.push(Active { v, raw_pos, env_state: None });
                    } else {
                        i += 1;
                    }
                }
            }
        }

        let song_volume = self.song_volume;
        let linear = self.linear;
        let pause_fade = self.pause_fade;

        {
            let Self { active, shim_by_line, shim_scratch, .. } = self;
            active.retain_mut(|a| {
                if !mute::is_note_active(&a.v.part_path, a.v.line as usize, a.v.source_row as i64) {
                    return false;
                }
                let v = &a.v;
                let smp = &*v.sample;
                let len = smp.len() as f64;
                let const_step = smp.rate * (v.pitch_hz / C5_HZ) / sr;
                let loop_f = match smp.loop_pts {
                    Some((s, e)) if e > s => Some((s as f64, e as f64)),
                              _ => None,
                };
                let automated_gain = v.vol_lane.is_some() || v.pan_lane.is_some();

                let release_sec = a.env_state.as_ref().map_or(0.020, |s| s.release_end - s.release_start);
                let actual_end = v.end + (release_sec * sr) as i64;

                let (i_lo, i_hi) = if speed > 0 {
                    let lo = ((v.start - block_start) as f64 / speed as f64)
                    .ceil()
                    .max(0.0) as i64;
                    let hi = ((actual_end - block_start) as f64 / speed as f64)
                    .ceil()
                    .min(block_len as f64) as i64;
                    (lo, hi)
                } else if speed < 0 {
                    let lo = (((v.end - block_start) as f64 / speed as f64)
                    .floor() as i64) + 1;
                    let hi = (((v.start - block_start) as f64 / speed as f64)
                    .floor() as i64) + 1;
                    (lo.max(0), hi.min(block_len))
                } else {
                    (0, block_len)
                };

                let first_shim: Option<u32> =
                shim_by_line.get(&v.line).and_then(|s| s.first().copied());

                let part_vol = crate::mixer::part_volume::get(&v.part_path);

                let mut emit = |o: usize, cl: f32, cr: f32| {
                    match first_shim.and_then(|id| shim_scratch.get_mut(&id)) {
                        Some(buf) if o < buf.0.len() => {
                            buf.0[o] += cl;
                            buf.1[o] += cr;
                        }
                        _ => {
                            out_l[o] += cl;
                            out_r[o] += cr;
                        }
                    }
                };

                for i in i_lo..i_hi {
                    let f = block_start + i * speed;
                    let t = f as f64 / sr;
                    let step = match &v.pitch_lane {
                        Some(l) => smp.rate * (l.value_at(t) / C5_HZ) / sr,
                              None => const_step,
                    };

                    let mut p = a.raw_pos;
                    if let Some((ls, le)) = loop_f {
                        let ll = le - ls;
                        if ll > 0.0 {
                            if p >= le {
                                p = ls + (p - le).rem_euclid(ll);
                            } else if p < ls {
                                p = le - (ls - p).rem_euclid(ll);
                            }
                        }
                    }

                    let gain_pair = || -> (f64, f64) {
                        let (gl, gr) = if automated_gain {
                            let vol = v.vol_lane.as_ref().map_or(v.vol, |l| l.value_at(t));
                            let pan = v.pan_lane.as_ref().map_or(v.pan, |l| l.value_at(t));
                            gains(vol, pan, song_volume, linear)
                        } else {
                            v.const_gain
                        };
                        (gl * part_vol, gr * part_vol)
                    };

                    let env_gain = || -> f32 {
                        let mut g = 1.0;
                        if speed == 0 {
                            g *= pause_fade;
                        }
                        match &a.env_state {
                            Some(state) => {
                                let t_sec = (f - state.start_sample) as f64 / sr;
                                g * env_level_at(state, t_sec)
                            }
                            None => {
                                let t_sec = (f - v.start) as f64 / sr;
                                let note_dur = (v.end - v.start) as f64 / sr;
                                if t_sec > note_dur {
                                    let over = (t_sec - note_dur) as f32;
                                    g * (1.0 - over / 0.020).clamp(0.0, 1.0)
                                } else {
                                    g
                                }
                            }
                        }
                    };

                    if p >= 0.0 && p < len {
                        let (sl, sr_) = interpolate(smp, p, step, a.raw_pos);
                        let (gl, gr) = gain_pair();
                        let eg = env_gain();
                        let o = i as usize;
                        let cl = sl * gl as f32 * eg;
                        let cr = sr_ * gr as f32 * eg;
                        emit(o, cl, cr);
                    } else if p >= len && loop_f.is_none() && len > 0.0 {
                        let over = p - len;
                        let fade_len = HOLD_TAIL_SECS * sr;
                        if over < fade_len {
                            let fade = (1.0 - (over / fade_len)).clamp(0.0, 1.0) as f32;
                            let last_i = (len as usize).saturating_sub(1);
                            let sl = smp.left[last_i];
                            let sr_ = if smp.stereo { smp.right[last_i] } else { sl };
                            let (gl, gr) = gain_pair();
                            let eg = env_gain();
                            let o = i as usize;
                            let cl = sl * gl as f32 * eg * fade;
                            let cr = sr_ * gr as f32 * eg * fade;
                            emit(o, cl, cr);
                        }
                    }

                    a.raw_pos += step * dir_f;
                }

                if speed > 0 {
                    actual_end > block_end
                } else if speed < 0 {
                    block_end > v.start
                } else {
                    true
                }
            });
        }

        if !self.hosts.is_empty() || !self.sf2_engines.is_empty() {
            if speed == 1 {
                // If we are resuming, resync the VSTs to the exact playhead
                if !self.vst_synced {
                    self.resync_vsts(block_start);
                }
                self.render_vsts(out_l, out_r, block_start);
                self.vst_hist_head = self.vst_hist_head.max(block_end - 1);
            } else if speed == 0 {
                // If we are paused, DO NOT panic_hosts() or clear the VSTs.
                // Just let them play into the master fade.
                self.vst_synced = false;
                self.render_vsts(out_l, out_r, block_start);
            } else {
                if self.vst_synced {
                    self.panic_hosts();
                    self.vst_synced = false;
                }
                self.render_vsts_history(out_l, out_r, block_start, speed);
            }
        }

        if !self.shims.is_empty() {
            self.process_shims(out_l, out_r);
        }

        sanitize(out_l);
        sanitize(out_r);

        if let Some(chain) = self.master_fx.as_mut() {
            if !chain.is_empty() {
                let mut i = 0;
                while i < out_l.len() {
                    let n = (out_l.len() - i).min(VST_BLOCK as usize);
                    chain.process(&mut out_l[i..i + n], &mut out_r[i..i + n]);
                    i += n;
                }
            }
        }

        sanitize(out_l);
        sanitize(out_r);

        if speed != 1 && speed != 0 {
            let scale = 1.0 / (speed.unsigned_abs() as f32);
            for s in out_l.iter_mut() { *s *= scale; }
            for s in out_r.iter_mut() { *s *= scale; }
        }

        // Handle Pause Tail decrement and Final Fade
        if speed == 0 {
            self.tail_frames -= block_len;

            // Fade raw samples over 100ms so they don't drone
            self.pause_fade = (self.pause_fade - (block_len as f32 / (0.1 * sr) as f32)).max(0.0);

            // Gently fade out the entire master bus (including VSTs/reverbs)
            // over the entire 1-second tail.
            let fade_frames = (self.sr * 1.0) as i64;
            if self.tail_frames < fade_frames {
                let fade = (self.tail_frames as f32 / fade_frames as f32).clamp(0.0, 1.0);
                for i in 0..out_l.len() {
                    out_l[i] *= fade;
                    out_r[i] *= fade;
                }
            }
        } else {
            self.pos = block_end.clamp(0, self.total);
        }
    }

    fn process_shims(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let len = out_l.len();

        if self.shim_dry_l.len() < len {
            self.shim_dry_l.resize(len, 0.0);
            self.shim_dry_r.resize(len, 0.0);
        }
        for buf in self.shim_scratch.values_mut() {
            if buf.0.len() < len {
                buf.0.resize(len, 0.0);
                buf.1.resize(len, 0.0);
            }
        }

        let mut dry_l = std::mem::take(&mut self.shim_dry_l);
        let mut dry_r = std::mem::take(&mut self.shim_dry_r);

        let block = VST_BLOCK as usize;
        let num_chains = self.shim_chains_cache.len();

        let mut chunk_start = 0usize;
        while chunk_start < len {
            let chunk_end = (chunk_start + block).min(len);
            let chunk_len = chunk_end - chunk_start;

            if self.shim_link_l.len() < chunk_len {
                self.shim_link_l.resize(chunk_len, 0.0);
                self.shim_link_r.resize(chunk_len, 0.0);
            }
            self.shim_processed.clear();

            for chain_idx in 0..num_chains {
                let chain_len = self.shim_chains_cache[chain_idx].len();
                for i in 0..chain_len {
                    let shim_id = self.shim_chains_cache[chain_idx][i];
                    if self.shim_processed.contains(&shim_id) {
                        continue;
                    }
                    self.shim_processed.push(shim_id);

                    let Some(shim) = self.shims.get_mut(&shim_id) else { continue; };
                    let Some(buf) = self.shim_scratch.get_mut(&shim_id) else { continue; };

                    dry_l[chunk_start..chunk_end]
                    .copy_from_slice(&buf.0[chunk_start..chunk_end]);
                    dry_r[chunk_start..chunk_end]
                    .copy_from_slice(&buf.1[chunk_start..chunk_end]);

                    shim.host.process_inplace(
                        &mut buf.0[chunk_start..chunk_end],
                        &mut buf.1[chunk_start..chunk_end],
                    );

                    let mix = shim.mix as f32;
                    let dry_amt = 1.0 - mix;
                    let theta = ((shim.pan + 1.0) * std::f64::consts::FRAC_PI_4) as f32;
                    let gl = (shim.gain as f32) * theta.cos();
                    let gr = (shim.gain as f32) * theta.sin();

                    for j in chunk_start..chunk_end {
                        buf.0[j] = dry_l[j] * dry_amt + buf.0[j] * mix;
                        buf.1[j] = dry_r[j] * dry_amt + buf.1[j] * mix;
                        buf.0[j] *= gl;
                        buf.1[j] *= gr;
                    }

                    if let Some(next_id) = self.shim_chains_cache[chain_idx].get(i + 1).copied() {
                        {
                            let Some(buf) = self.shim_scratch.get(&shim_id) else { continue; };
                            self.shim_link_l[..chunk_len]
                            .copy_from_slice(&buf.0[chunk_start..chunk_end]);
                            self.shim_link_r[..chunk_len]
                            .copy_from_slice(&buf.1[chunk_start..chunk_end]);
                        }
                        if let Some(next) = self.shim_scratch.get_mut(&next_id) {
                            for j in 0..chunk_len {
                                next.0[chunk_start + j] += self.shim_link_l[j];
                                next.1[chunk_start + j] += self.shim_link_r[j];
                            }
                        }
                    } else if let Some(b) = self.shim_scratch.get(&shim_id) {
                        for j in chunk_start..chunk_end {
                            out_l[j] += b.0[j];
                            out_r[j] += b.1[j];
                        }
                    }
                }
            }

            chunk_start = chunk_end;
        }

        self.shim_dry_l = dry_l;
        self.shim_dry_r = dry_r;
    }

    fn render_vsts(&mut self, out_l: &mut [f32], out_r: &mut [f32], block_start: i64) {
        let block_len = out_l.len() as i64;
        let block_end = block_start + block_len;

        let Self {
            hosts,
            host_scratch,
            vst_gains,
            iid_to_part_path,
            vst_pending,
            vst_active,
            vst_held,
            vst_scratch_l,
            vst_scratch_r,
            vst_hist_l,
            vst_hist_r,
            sf2_engines,
            insert_chains,
            channel_to_insert,
            insert_scratch,
            sf2_scratch,
            vst_events_buf,
            ..
        } = self;

        for buf in insert_scratch.values_mut() {
            if buf.0.len() < VST_BLOCK as usize {
                buf.0.resize(VST_BLOCK as usize, 0.0);
                buf.1.resize(VST_BLOCK as usize, 0.0);
            }
        }

        let mut chunk_start = block_start;
        while chunk_start < block_end {
            let chunk_len = ((block_end - chunk_start) as usize).min(VST_BLOCK as usize);
            let chunk_end = chunk_start + chunk_len as i64;
            vst_events_buf.clear();
            let events = &mut *vst_events_buf;

            while vst_pending.front().map_or(false, |n| n.start < chunk_end) {
                let n = vst_pending.pop_front().unwrap();
                if !crate::mixer::mute::is_note_active(&n.part_path, n.line as usize, n.source_row as i64) {
                    continue;
                }
                let off = (n.start - chunk_start).max(0) as i32;
                if let Some(i) = vst_active.iter().position(|a| a.iid == n.iid && a.note == n.note) {
                    vst_active.remove(i);
                    events.push((off, 0, n.iid, n.note, 0));
                }
                events.push((off, 1, n.iid, n.note, n.velocity));
                vst_active.push(n);
            }

            let mut i = 0;
            while i < vst_active.len() {
                if vst_active[i].end < chunk_end {
                    let n = vst_active.remove(i);
                    let off = (n.end - chunk_start).max(0) as i32;
                    events.push((off, 0, n.iid, n.note, 0));
                } else {
                    i += 1;
                }
            }

            events.sort_unstable_by_key(|e| (e.0, e.1));
            for &(off, kind, iid, note, vel) in events.iter() {
                if let Some(host) = hosts.get_mut(&iid) {
                    let bit = 1u128 << (note.min(127) as u32);
                    let held = vst_held.entry(iid).or_insert(0);
                    if kind == 1 {
                        host.note_on_at(note, vel, off);
                        *held |= bit;
                    } else {
                        host.note_off_at(note, off);
                        *held &= !bit;
                    }
                } else if let Some(eng) = sf2_engines.get_mut(&iid) {
                    if kind == 1 {
                        eng.note_on_at(note, vel, off);
                    } else {
                        eng.note_off_at(note, off);
                    }
                }
            }

            vst_scratch_l[..chunk_len].fill(0.0);
            vst_scratch_r[..chunk_len].fill(0.0);
            for buf in insert_scratch.values_mut() {
                buf.0[..chunk_len].fill(0.0);
                buf.1[..chunk_len].fill(0.0);
            }

            for (iid, host) in hosts.iter_mut() {
                let Some((tl, tr)) = host_scratch.get_mut(iid) else { continue };
                tl[..chunk_len].fill(0.0);
                tr[..chunk_len].fill(0.0);
                host.process(&mut tl[..chunk_len], &mut tr[..chunk_len]);

                let host_gain = vst_gains.get(iid).copied().unwrap_or(1.0);
                let part_scale = iid_to_part_path
                .get(iid)
                .map(|p| crate::mixer::part_volume::get(p) as f32)
                .unwrap_or(1.0);
                let gain = host_gain * part_scale;
                super::scope::scope_push_block(
                    *iid as usize,
                    &tl[..chunk_len],
                    &tr[..chunk_len],
                    gain,
                );
                let insert = channel_to_insert.get(iid).copied();

                match insert.and_then(|ins| insert_scratch.get_mut(&ins)) {
                    Some(buf) => mix_add(&mut buf.0[..chunk_len], &mut buf.1[..chunk_len], &tl[..chunk_len], &tr[..chunk_len], gain),
                    None => mix_add(&mut vst_scratch_l[..chunk_len], &mut vst_scratch_r[..chunk_len], &tl[..chunk_len], &tr[..chunk_len], gain),
                }
            }

            for (iid, eng) in sf2_engines.iter_mut() {
                let (sl, sr) = sf2_scratch
                .entry(*iid)
                .or_insert_with(|| (vec![0.0; VST_BLOCK as usize], vec![0.0; VST_BLOCK as usize]));
                if sl.len() < chunk_len {
                    sl.resize(chunk_len, 0.0);
                    sr.resize(chunk_len, 0.0);
                }
                sl[..chunk_len].fill(0.0);
                sr[..chunk_len].fill(0.0);
                eng.process(&mut sl[..chunk_len], &mut sr[..chunk_len]);

                let part_scale = iid_to_part_path
                .get(iid)
                .map(|p| crate::mixer::part_volume::get(p) as f32)
                .unwrap_or(1.0);
                super::scope::scope_push_block(
                    *iid as usize,
                    &sl[..chunk_len],
                    &sr[..chunk_len],
                    part_scale,
                );

                let insert = channel_to_insert.get(iid).copied();
                match insert.and_then(|ins| insert_scratch.get_mut(&ins)) {
                    Some(buf) => mix_add(&mut buf.0[..chunk_len], &mut buf.1[..chunk_len], &sl[..chunk_len], &sr[..chunk_len], part_scale),
                    None => mix_add(&mut vst_scratch_l[..chunk_len], &mut vst_scratch_r[..chunk_len], &sl[..chunk_len], &sr[..chunk_len], part_scale),
                }
            }

            for (&insert, chain) in insert_chains.iter_mut() {
                if chain.is_empty() { continue; }
                if let Some(buf) = insert_scratch.get_mut(&insert) {
                    chain.process(&mut buf.0[..chunk_len], &mut buf.1[..chunk_len]);
                    mix_add(&mut vst_scratch_l[..chunk_len], &mut vst_scratch_r[..chunk_len], &buf.0[..chunk_len], &buf.1[..chunk_len], 1.0);
                }
            }

            let out_off = (chunk_start - block_start) as usize;
            mix_add(&mut out_l[out_off..out_off + chunk_len], &mut out_r[out_off..out_off + chunk_len], &vst_scratch_l[..chunk_len], &vst_scratch_r[..chunk_len], 1.0);

            if !vst_hist_l.is_empty() {
                for j in 0..chunk_len {
                    let pos = chunk_start + j as i64;
                    let idx = pos.rem_euclid(vst_hist_l.len() as i64) as usize;
                    vst_hist_l[idx] = vst_scratch_l[j];
                    vst_hist_r[idx] = vst_scratch_r[j];
                }
            }

            chunk_start = chunk_end;
        }
    }

    fn render_vsts_history(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        block_start: i64,
        speed: i64,
    ) {
        let head = self.vst_hist_head;
        if head == i64::MIN {
            return;
        }
        let len = self.vst_hist_l.len() as i64;
        if len == 0 {
            return;
        }
        let frames = out_l.len() as i64;
        for i in 0..frames {
            let pos = block_start + i * speed;
            if pos > head || pos <= head - len {
                continue;
            }
            let idx = pos.rem_euclid(len) as usize;
            out_l[i as usize] += self.vst_hist_l[idx];
            out_r[i as usize] += self.vst_hist_r[idx];
        }
    }
}
