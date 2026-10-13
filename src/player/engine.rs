//! Voice scheduling, mixer, and DSP primitives.

use std::collections::{BTreeMap, VecDeque};
use std::f64::consts::{FRAC_PI_4, PI};
use std::sync::{Arc, OnceLock};

use crate::effects::EffectChain;
use crate::sources::trackscript::C5_HZ;
use crate::timing::AutomationLane;
use crate::tui::mute;
use crate::vst_host_ffi::VstHost;

use super::project::{Project, Sample, Sf2VoiceEnvelope};

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
    /// Identity for mute lookups.  `part_path` is the string
    /// `Project::part_path(name)` returns; `source_row` is the note's
    /// row within its own part.  Together with `line` (the block line
    /// the voice was placed on) these form the key the UI toggles.
    pub(crate) part_path: String,
    pub(crate) source_row: f64,
    /// SF2 volume envelope for this voice.  `None` for voices that play
    /// at constant gain (WAV samples, embedded audio, WAV-based multi
    /// samples).
    pub(crate) env: Option<Sf2VoiceEnvelope>,
}

impl Scheduled {
    fn key(&self) -> (i64, u32, u32) {
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
    /// Same identity scheme as `Scheduled`.
    pub(crate) part_path: String,
    pub(crate) source_row: f64,
}

impl VstNote {
    fn key(&self) -> (i64, u32, u32) {
        (self.start, self.line, self.seq)
    }
}

// ── mixing maths ───────────────────────────────────────────────────────

#[inline]
pub(crate) fn gains(vol_pct: f64, pan: f64, song_volume: f64, linear: bool) -> (f64, f64) {
    let v = vol_pct * song_volume / 100.0;
    let pan = pan.clamp(-1.0, 1.0);
    if linear {
        // Balance-style law: unity gain on both sides at centre, the far
        // side fades to silence as the pan moves out (peaks at 0 dB, no
        // centre dip).  Centred sources play at full level, in line with
        // synths, whose output bypasses the pan law.
        (v * (1.0 - pan).min(1.0), v * (1.0 + pan).min(1.0))
    } else {
        let theta = (pan + 1.0) * FRAC_PI_4;
        (v * theta.cos(), v * theta.sin())
    }
}

// ── sinc interpolation ─────────────────────────────────────────────────

const SINC_PHASES: usize = 512;
const SINC_TAPS: usize = 8;
const SINC_TAPS_BEFORE: i64 = 3;

static SINC_UP: OnceLock<[[f64; SINC_TAPS]; SINC_PHASES]> = OnceLock::new();

#[inline]
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = PI * x;
        px.sin() / px
    }
}

fn up_table() -> &'static [[f64; SINC_TAPS]; SINC_PHASES] {
    SINC_UP.get_or_init(|| {
        let mut t = [[0.0f64; SINC_TAPS]; SINC_PHASES];
        for p in 0..SINC_PHASES {
            let frac = p as f64 / SINC_PHASES as f64;
            let mut sum = 0.0;
            for k in 0..SINC_TAPS {
                let d = frac - (k as i64 - SINC_TAPS_BEFORE) as f64;
                let win = if d.abs() < 4.0 {
                    0.5 + 0.5 * (PI * d / 4.0).cos()
                } else {
                    0.0
                };
                t[p][k] = sinc(d) * win;
                sum += t[p][k];
            }
            if sum.abs() >= 1e-12 {
                for k in 0..SINC_TAPS {
                    t[p][k] /= sum;
                }
            }
        }
        t
    })
}

fn interpolate(smp: &Sample, pos: f64, step: f64, raw_pos: f64) -> (f32, f32) {
    let n = smp.len() as i64;
    let center = pos.floor() as i64;

    if step <= 1.0 {
        let frac = pos - center as f64;
        let phase = ((frac * SINC_PHASES as f64) as usize) & (SINC_PHASES - 1);
        let w = &up_table()[phase];

        let mut l = 0.0f64;
        let mut r = 0.0f64;
        for k in 0..SINC_TAPS {
            let mut i = center + (k as i64 - SINC_TAPS_BEFORE);
            if let Some((ls, le)) = smp.loop_pts {
                let ll = le - ls;
                if ll > 0 {
                    let wrap = i >= le || (raw_pos >= le as f64 && i < ls);
                    if wrap {
                        i = ls + (i - ls).rem_euclid(ll);
                    }
                }
            }
            let i = i.clamp(0, n - 1) as usize;
            l += smp.left[i] as f64 * w[k];
            if smp.stereo {
                r += smp.right[i] as f64 * w[k];
            }
        }
        if !smp.stereo {
            r = l;
        }
        return (l as f32, r as f32);
    }

    let mut i0 = center;
    let mut i1 = center + 1;
    if let Some((ls, le)) = smp.loop_pts {
        let ll = le - ls;
        if ll > 0 {
            let wrap0 = i0 >= le || (raw_pos >= le as f64 && i0 < ls);
            let wrap1 = i1 >= le || (raw_pos >= le as f64 && i1 < ls);
            if wrap0 {
                i0 = ls + (i0 - ls).rem_euclid(ll);
            }
            if wrap1 {
                i1 = ls + (i1 - ls).rem_euclid(ll);
            }
        }
    }
    let i0 = i0.clamp(0, n - 1) as usize;
    let i1 = i1.clamp(0, n - 1) as usize;
    let frac = pos - center as f64;

    let l0 = smp.left[i0] as f64;
    let l1 = smp.left[i1] as f64;
    let l = l0 + (l1 - l0) * frac;

    let r = if smp.stereo {
        let r0 = smp.right[i0] as f64;
        let r1 = smp.right[i1] as f64;
        r0 + (r1 - r0) * frac
    } else {
        l
    };

    (l as f32, r as f32)
}

// ── envelope phase boundaries ──────────────────────────────────────────

/// Pre-computed envelope phase boundaries for one active SF2 voice.
/// All times are in seconds from the voice's scheduled start.
struct EnvState {
    attack_end: f64,
    hold_end: f64,
    decay_end: f64,
    /// When the release phase begins (seconds from start).
    release_start: f64,
    /// When the voice is fully silent (seconds from start).
    release_end: f64,
    /// Peak amplitude (from attenuation + velocity).
    peak: f32,
    /// Sustain level as an absolute amplitude (peak * fraction).
    sustain: f32,
    attack_inv: f32,
    decay_inv: f32,
    release_inv: f32,
    /// Absolute sample position of note-on, for `t = (f - start) / sr`.
    start_sample: i64,
}

#[inline]
fn env_level_at(state: &EnvState, t_sec: f64) -> f32 {
    if t_sec < 0.0 {
        return 0.0;
    }
    if t_sec >= state.release_end {
        return 0.0;
    }
    if t_sec >= state.release_start {
        // Release: ramp from the level at release_start down to zero.
        let at_release = env_sustain_level(state, state.release_start);
        let ft = ((t_sec - state.release_start) * state.release_inv as f64)
            .clamp(0.0, 1.0) as f32;
        return at_release * (1.0 - ft);
    }
    env_sustain_level(state, t_sec)
}

#[inline]
fn env_sustain_level(state: &EnvState, t_sec: f64) -> f32 {
    if t_sec < state.attack_end {
        if state.attack_end <= 0.0 {
            return state.peak;
        }
        return (t_sec * state.attack_inv as f64) as f32 * state.peak;
    }
    if t_sec < state.hold_end {
        return state.peak;
    }
    if t_sec < state.decay_end {
        let f = ((t_sec - state.hold_end) * state.decay_inv as f64).clamp(0.0, 1.0) as f32;
        return state.peak - (state.peak - state.sustain) * f;
    }
    state.sustain
}

// ── engine ─────────────────────────────────────────────────────────────

struct Active {
    v: Scheduled,
    raw_pos: f64,
    env_state: Option<EnvState>,
}

pub(crate) enum Msg {
    Batch { epoch: u64, samples: Vec<Scheduled>, vsts: Vec<VstNote> },
    Seek  { epoch: u64, pos: i64, samples: Vec<Scheduled>, vsts: Vec<VstNote> },
}

const VST_BLOCK: i64 = 512;

const VST_HISTORY_SECS: f64 = 10.0;

/// How long the "hold last sample value" tail lasts for one-shot SF2
/// voices.  Bounded so DC can't accumulate: a fixed output of a
/// non-zero sample feeds any downstream feedback effect, and the
/// master reverb would grow without bound.  20 ms is long enough to
/// smooth a click, short enough to be inaudible as DC.
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

    hosts: BTreeMap<u16, VstHost>,
    host_scratch: BTreeMap<u16, (Vec<f32>, Vec<f32>)>,
    /// Post-plugin output gain per host iid.  Missing = 1.0.
    vst_gains: BTreeMap<u16, f32>,
    vst_pending: VecDeque<VstNote>,
    vst_active: Vec<VstNote>,
    /// Every VST note ingested since the last seek, unclamped.  Used to
    /// rebuild `vst_pending` whenever playback leaves and rejoins normal
    /// forward speed (reverse, fast-forward), so no notes are lost.
    vst_log: Vec<VstNote>,
    /// Keys each host has been told are on (bit n = MIDI key n).  The
    /// source of truth for "what might be sounding", independent of
    /// `vst_active`, so a panic can silence exactly those keys.
    vst_held: BTreeMap<u16, u128>,
    /// True while the hosts are following the timeline (speed == 1).
    vst_synced: bool,
    vst_scratch_l: Vec<f32>,
    vst_scratch_r: Vec<f32>,

    vst_hist_l: Vec<f32>,
    vst_hist_r: Vec<f32>,
    vst_hist_head: i64,

    master_fx: Option<EffectChain>,
}

impl Engine {
    pub(crate) fn new(p: &Project) -> Self {
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
            master_fx: None,
        }
    }

    pub(crate) fn install_host(&mut self, iid: u16, host: VstHost) {
        self.install_host_with_gain(iid, host, 1.0);
    }

    /// Install a VST host with a post-plugin output gain.  Used for
    /// fallbacks whose default patch is louder than the plugin they
    /// stand in for.  The gain is applied to the plugin's audio output,
    /// so it works even on patches that ignore MIDI velocity.
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
    }

    pub(crate) fn host_count(&self) -> usize {
        self.hosts.len()
    }

    pub(crate) fn install_master_fx(&mut self, chain: EffectChain) {
        self.master_fx = Some(chain);
    }

    pub(crate) fn master_fx_count(&self) -> usize {
        self.master_fx.as_ref().map_or(0, |c| c.len())
    }

    pub(crate) fn sample_rate(&self) -> f64 { self.sr }

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

        let attack_end = env.attack_sec as f64;
        let hold_end = attack_end + env.hold_sec as f64;
        let decay_end = hold_end + env.decay_sec as f64;

        // Release occupies the last `release_sec` seconds of the note,
        // capped at half the note so short hits still have a body.
        let release_sec = (env.release_sec as f64).min(note_dur_sec * 0.5);
        let release_start = (note_dur_sec - release_sec).max(0.0);

        let attack_inv = if env.attack_sec > 0.0 {
            1.0 / env.attack_sec
        } else {
            0.0
        };
        let decay_inv = if env.decay_sec > 0.0 {
            1.0 / env.decay_sec
        } else {
            0.0
        };
        let release_inv = if release_sec > 0.0 {
            (1.0 / release_sec) as f32
        } else {
            0.0
        };

        let peak = env.peak.clamp(0.0, 1.0);
        let sustain = peak * env.sustain_fraction.clamp(0.0, 1.0);

        Some(EnvState {
            attack_end,
            hold_end,
            decay_end,
            release_start,
            release_end: note_dur_sec,
            peak,
            sustain,
            attack_inv,
            decay_inv,
            release_inv,
            start_sample: v.start,
        })
    }

    pub(crate) fn ingest(&mut self, mut voices: Vec<Scheduled>) {
        voices.sort_by_key(Scheduled::key);
        let mut future = Vec::new();
        for v in voices {
            if v.end <= self.pos {
                continue;
            }
            if v.start < self.pos {
                // Already-sounding voice being re-ingested, typically
                // after a seek or loop.  Check mute here too: the
                // activation check in `render` only runs for voices
                // that come out of `pending`, and these go straight
                // into `active`.  Without this, a muted note that was
                // sounding at the seek point gets re-activated and
                // plays out its remaining duration.
                if !mute::is_note_active(&v.part_path, v.line as usize, v.source_row as i64) {
                    continue;
                }
                let raw_pos = self.raw_pos_at(&v, self.pos);
                let env_state = self.env_state_for(&v);
                self.active.push(Active { v, raw_pos, env_state });
            } else {
                future.push(v);
            }
        }
        if !future.is_empty() {
            let mut all: Vec<Scheduled> = self.pending.drain(..).collect();
            all.extend(future);
            all.sort_by_key(Scheduled::key);
            self.pending = all.into();
        }
    }

    /// Queue VST notes.  Notes whose start is already in the past are
    /// pinned to `pos` and routed through `vst_pending` so the render
    /// loop fires their note-on at offset 0.  Without this, the plugin
    /// never learns the note started, and a later note-off can kill an
    /// unrelated voice that happens to share the same pitch.
    pub(crate) fn ingest_vst(&mut self, mut notes: Vec<VstNote>) {
        notes.sort_by_key(VstNote::key);
        self.vst_log.extend(notes.iter().cloned());
        let mut future = Vec::new();
        for mut n in notes {
            if n.end <= self.pos {
                continue;
            }
            if n.start < self.pos {
                n.start = self.pos;
            }
            future.push(n);
        }
        if !future.is_empty() {
            let mut all: Vec<VstNote> = self.vst_pending.drain(..).collect();
            all.extend(future);
            all.sort_by_key(VstNote::key);
            self.vst_pending = all.into();
        }
    }

    pub(crate) fn apply_seek(&mut self, to: i64, voices: Vec<Scheduled>) {
        self.pos = to.clamp(0, self.total);
        self.active.clear();
        self.pending.clear();
        // Release every key the hosts think is held.  The events queue
        // and fire on the next `process()` call, ahead of any chase
        // note-ons ingested below.
        self.panic_hosts();
        self.vst_pending.clear();
        self.vst_log.clear();
        self.vst_synced = true;
        self.vst_hist_head = i64::MIN;
        self.ready = true;
        self.ingest(voices);
    }

    /// Note-off for every key a host has been told is on, then forget
    /// them.  Safe to call at any time; keys that are not sounding are
    /// simply ignored by the plugin.
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
        self.vst_active.clear();
    }

    /// Rejoin the timeline at `pos` after reverse or fast-forward:
    /// silence everything, then rebuild the pending queue from the log.
    /// Notes already sounding at `pos` are chased (note-on at `pos`).
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

        if speed == 0 {
            return;
        }

        let block_len = out_l.len() as i64;
        let block_start = self.pos;
        let block_end = block_start + block_len * speed;
        let sr = self.sr;
        let dir: i64 = if speed > 0 { 1 } else { -1 };
        let dir_f = dir as f64;

        // ── Voice activation ─────────────────────────────────────────
        //
        // A scheduled voice becomes audible here.  Mute is checked at
        // this point so toggling mute mid-song takes effect on the very
        // next note event.  Muted voices are dropped silently: they
        // never enter `active`.
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
                    // No envelope in reverse: keep the pre-existing
                    // const-gain behavior so reversing doesn't
                    // produce a ghost envelope tail.
                    self.active.push(Active { v, raw_pos, env_state: None });
                } else {
                    i += 1;
                }
            }
        }

        // ── Sample voices ────────────────────────────────────────────
        let song_volume = self.song_volume;
        let linear = self.linear;

        self.active.retain_mut(|a| {
            // Instant mute: drop voices whose key became muted since
            // the last block.  This is what makes muting a note that's
            // mid-flight stop the sound on the very next audio buffer
            // instead of waiting for the note to finish on its own.
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

            let (i_lo, i_hi) = if speed > 0 {
                let lo = ((v.start - block_start) as f64 / speed as f64)
                    .ceil()
                    .max(0.0) as i64;
                let hi = ((v.end - block_start) as f64 / speed as f64)
                    .ceil()
                    .min(block_len as f64) as i64;
                (lo, hi)
            } else {
                let lo = (((v.end - block_start) as f64 / speed as f64)
                    .floor() as i64)
                    + 1;
                let hi = (((v.start - block_start) as f64 / speed as f64)
                    .floor() as i64)
                    + 1;
                (lo.max(0), hi.min(block_len))
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
                    if automated_gain {
                        let vol = v.vol_lane.as_ref().map_or(v.vol, |l| l.value_at(t));
                        let pan = v.pan_lane.as_ref().map_or(v.pan, |l| l.value_at(t));
                        gains(vol, pan, song_volume, linear)
                    } else {
                        v.const_gain
                    }
                };
                let env_gain = || -> f32 {
                    match &a.env_state {
                        Some(state) => {
                            let t_sec = (f - state.start_sample) as f64 / sr;
                            env_level_at(state, t_sec)
                        }
                        None => 1.0,
                    }
                };

                if p >= 0.0 && p < len {
                    // Normal playback: read interpolated sample data.
                    let (sl, sr_) = interpolate(smp, p, step, a.raw_pos);
                    let (gl, gr) = gain_pair();
                    let eg = env_gain();
                    let o = i as usize;
                    out_l[o] += sl * gl as f32 * eg;
                    out_r[o] += sr_ * gr as f32 * eg;
                } else if p >= len && loop_f.is_none() && a.env_state.is_some() && len > 0.0 {
                    // One-shot SF2 sample has ended before the note
                    // did.  Hold the final value for a short fade
                    // window, then silence.  The window is bounded
                    // (HOLD_TAIL_SECS) so we never emit DC for longer
                    // than that — a fixed non-zero output feeds any
                    // downstream feedback effect, and the master
                    // reverb would grow without bound.
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
                        out_l[o] += sl * gl as f32 * eg * fade;
                        out_r[o] += sr_ * gr as f32 * eg * fade;
                    }
                    // Else: beyond the hold tail — silent.
                }
                // Else: p < 0 (before start) — silent.

                a.raw_pos += step * dir_f;
            }

            if speed > 0 {
                v.end > block_end
            } else {
                block_end > v.start
            }
        });

        // ── VST voices ───────────────────────────────────────────────
        if !self.hosts.is_empty() {
            if speed == 1 {
                if !self.vst_synced {
                    self.resync_vsts(block_start);
                }
                self.render_vsts(out_l, out_r, block_start);
                self.vst_hist_head = self.vst_hist_head.max(block_end - 1);
            } else {
                if self.vst_synced {
                    self.panic_hosts();
                    self.vst_synced = false;
                }
                self.render_vsts_history(out_l, out_r, block_start, speed);
            }
        }

        // ── NaN / inf guard (pre-FX) ─────────────────────────────────
        // A single NaN anywhere in the mix poisons everything
        // downstream.  Catch it before the master chain so a bad
        // sample or a runaway envelope can't corrupt the reverb state.
        for s in out_l.iter_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }
        for s in out_r.iter_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }

        // ── Master bus effects ───────────────────────────────────────
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

        // ── NaN / inf guard (post-FX) ────────────────────────────────
        // The reverb's feedback delay network can amplify a DC offset
        // into infinity over time.  Catch it here so the buffer
        // handed to the audio device is always finite.
        for s in out_l.iter_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }
        for s in out_r.iter_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }

        // ── Monitoring scale ─────────────────────────────────────────
        if speed != 1 {
            let scale = 1.0 / (speed.unsigned_abs() as f32);
            for s in out_l.iter_mut() { *s *= scale; }
            for s in out_r.iter_mut() { *s *= scale; }
        }

        self.pos = block_end.clamp(0, self.total);
    }

    fn render_vsts(&mut self, out_l: &mut [f32], out_r: &mut [f32], block_start: i64) {
        let block_len = out_l.len() as i64;
        let mut chunk_start = block_start;
        let block_end = block_start + block_len;

        let Self {
            hosts,
            host_scratch,
            vst_gains,
            vst_pending,
            vst_active,
            vst_held,
            vst_scratch_l,
            vst_scratch_r,
            vst_hist_l,
            vst_hist_r,
            ..
        } = self;

        // (offset, kind, iid, note, velocity); kind 0 = off, 1 = on.
        let mut events: Vec<(i32, u8, u16, u8, u8)> = Vec::new();

        while chunk_start < block_end {
            let chunk_len =
                ((block_end - chunk_start) as usize).min(VST_BLOCK as usize);
            let chunk_end = chunk_start + chunk_len as i64;
            events.clear();

            // Note-ons due in this chunk.
            while vst_pending
                .front()
                .map_or(false, |n| n.start < chunk_end)
            {
                let n = vst_pending.pop_front().unwrap();
                // Muted: skip the note-on entirely, and don't add it to
                // `vst_active` so no dangling note-off is generated.
                if !mute::is_note_active(&n.part_path, n.line as usize, n.source_row as i64) {
                    continue;
                }
                let off = (n.start - chunk_start).max(0) as i32;
                if let Some(i) = vst_active
                    .iter()
                    .position(|a| a.iid == n.iid && a.note == n.note)
                {
                    vst_active.remove(i);
                    events.push((off, 0, n.iid, n.note, 0));
                }
                events.push((off, 1, n.iid, n.note, n.velocity));
                vst_active.push(n);
            }

            // Note-offs due in this chunk.
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

            // Fire in sample order; at equal offsets note-offs go first.
            events.sort_by_key(|e| (e.0, e.1));
            for &(off, kind, iid, note, vel) in &events {
                let Some(host) = hosts.get_mut(&iid) else { continue };
                let bit = 1u128 << (note.min(127) as u32);
                let held = vst_held.entry(iid).or_insert(0);
                if kind == 1 {
                    host.note_on_at(note, vel, off);
                    *held |= bit;
                } else {
                    host.note_off_at(note, off);
                    *held &= !bit;
                }
            }

            for s in vst_scratch_l[..chunk_len].iter_mut() {
                *s = 0.0;
            }
            for s in vst_scratch_r[..chunk_len].iter_mut() {
                *s = 0.0;
            }

            for (iid, host) in hosts.iter_mut() {
                let Some((tl, tr)) = host_scratch.get_mut(iid) else { continue };
                for s in tl[..chunk_len].iter_mut() { *s = 0.0; }
                for s in tr[..chunk_len].iter_mut() { *s = 0.0; }
                host.process(&mut tl[..chunk_len], &mut tr[..chunk_len]);

                let gain = vst_gains.get(iid).copied().unwrap_or(1.0);

                let slot = *iid as usize;
                for j in 0..chunk_len {
                    let mono = (tl[j] + tr[j]) * 0.5 * gain;
                    crate::tui::scope_push(slot, mono);
                }
                for j in 0..chunk_len {
                    vst_scratch_l[j] += tl[j] * gain;
                    vst_scratch_r[j] += tr[j] * gain;
                }
            }

            if !vst_hist_l.is_empty() {
                for j in 0..chunk_len {
                    let pos = chunk_start + j as i64;
                    let idx = pos.rem_euclid(vst_hist_l.len() as i64) as usize;
                    vst_hist_l[idx] = vst_scratch_l[j];
                    vst_hist_r[idx] = vst_scratch_r[j];
                }
            }

            let out_off = (chunk_start - block_start) as usize;
            for j in 0..chunk_len {
                out_l[out_off + j] += vst_scratch_l[j];
                out_r[out_off + j] += vst_scratch_r[j];
            }

            chunk_start = chunk_end;
        }
    }

    /// Play back the recorded VST history at whatever rate the transport
    /// is running.  Used for both reverse and fast-forward.
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