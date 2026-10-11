//! Voice scheduling, mixer, and DSP primitives.

use std::collections::{HashMap, VecDeque};
use std::f64::consts::{FRAC_PI_4, PI};
use std::sync::{Arc, OnceLock};

use crate::effects::EffectChain;
use crate::sources::trackscript::C5_HZ;
use crate::timing::AutomationLane;
use crate::vst_host_ffi::VstHost;

use super::project::{Project, Sample};

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
}

impl Scheduled {
    fn key(&self) -> (i64, u32, u32) {
        (self.start, self.line, self.seq)
    }
}

// ── scheduled VST note ─────────────────────────────────────────────────

/// A single MIDI note destined for one channel's VST host.
#[derive(Clone, Debug)]
pub(crate) struct VstNote {
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) iid: u16,
    pub(crate) note: u8,
    pub(crate) velocity: u8,
    pub(crate) line: u32,
    pub(crate) seq: u32,
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
        (v * (1.0 - pan) * 0.5, v * (1.0 + pan) * 0.5)
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

// ── engine ─────────────────────────────────────────────────────────────

struct Active {
    v: Scheduled,
    raw_pos: f64,
}

pub(crate) enum Msg {
    Batch { epoch: u64, samples: Vec<Scheduled>, vsts: Vec<VstNote> },
    Seek  { epoch: u64, pos: i64, samples: Vec<Scheduled>, vsts: Vec<VstNote> },
}

/// Sample chunk used when driving VST hosts.  Notes are dispatched at
/// sample offsets within the chunk; the shim supports per-event offsets,
/// so this value controls the minimum scheduling granularity, not an
/// audible quantization.
const VST_BLOCK: i64 = 512;

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

    // VST hosting — one instance per channel, keyed by channel iid.
    hosts: HashMap<u16, VstHost>,
    vst_pending: VecDeque<VstNote>,
    vst_active: Vec<VstNote>,
    vst_scratch_l: Vec<f32>,
    vst_scratch_r: Vec<f32>,

    // Master bus effects — one chain, run on the fully-summed stereo mix.
    master_fx: Option<EffectChain>,
}

impl Engine {
    pub(crate) fn new(p: &Project) -> Self {
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
            hosts: HashMap::new(),
            vst_pending: VecDeque::new(),
            vst_active: Vec::new(),
            vst_scratch_l: vec![0.0; VST_BLOCK as usize],
            vst_scratch_r: vec![0.0; VST_BLOCK as usize],
            master_fx: None,
        }
    }

    /// Install a live VST host for one channel.  Called by the loader
    /// after resolving the channel's `.instrument` sidecar.
    pub(crate) fn install_host(&mut self, iid: u16, host: VstHost) {
        self.hosts.insert(iid, host);
    }

    pub(crate) fn host_count(&self) -> usize {
        self.hosts.len()
    }

    /// Attach a master-bus effect chain.  Runs after all voices are summed.
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

    pub(crate) fn ingest(&mut self, mut voices: Vec<Scheduled>) {
        voices.sort_by_key(Scheduled::key);
        let mut future = Vec::new();
        for v in voices {
            if v.end <= self.pos {
                continue;
            }
            if v.start < self.pos {
                let raw_pos = self.raw_pos_at(&v, self.pos);
                self.active.push(Active { v, raw_pos });
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

    /// Queue VST notes.  Notes that start before `pos` are pinned to `pos`
    /// so their `note_on` fires at seek time and the plugin plays from the
    /// seek point.
    pub(crate) fn ingest_vst(&mut self, mut notes: Vec<VstNote>) {
        notes.sort_by_key(VstNote::key);
        let mut future = Vec::new();
        for mut n in notes {
            if n.end <= self.pos {
                continue;
            }
            if n.start < self.pos {
                n.start = self.pos;
                self.vst_active.push(n);
            } else {
                future.push(n);
            }
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
        self.vst_active.clear();
        self.vst_pending.clear();
        self.ready = true;
        self.ingest(voices);
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

    pub(crate) fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        out_l.fill(0.0);
        out_r.fill(0.0);
        let block_start = self.pos;
        let block_end = block_start + out_l.len() as i64;
        let sr = self.sr;

        // ── sample voices ──────────────────────────────────────────────
        while self.pending.front().map_or(false, |v| v.start < block_end) {
            let v = self.pending.pop_front().unwrap();
            let raw_pos = v.offset;
            self.active.push(Active { v, raw_pos });
        }

        let song_volume = self.song_volume;
        let linear = self.linear;

        self.active.retain_mut(|a| {
            let v = &a.v;
            let lo = v.start.max(block_start);
            let hi = v.end.min(block_end);
            let smp = &*v.sample;
            let len = smp.len() as f64;
            let const_step = smp.rate * (v.pitch_hz / C5_HZ) / sr;
            let loop_f = match smp.loop_pts {
                Some((s, e)) if e > s => Some((s as f64, e as f64)),
                               _ => None,
            };
            let automated_gain = v.vol_lane.is_some() || v.pan_lane.is_some();

            for f in lo..hi {
                let t = f as f64 / sr;
                let step = match &v.pitch_lane {
                    Some(l) => smp.rate * (l.value_at(t) / C5_HZ) / sr,
                               None => const_step,
                };

                let mut p = a.raw_pos;
                if let Some((ls, le)) = loop_f {
                    if p >= le {
                        p = ls + (p - le).rem_euclid(le - ls);
                    }
                }

                if p >= 0.0 && p < len {
                    let (sl, sr_) = interpolate(smp, p, step, a.raw_pos);
                    let (gl, gr) = if automated_gain {
                        let vol = v.vol_lane.as_ref().map_or(v.vol, |l| l.value_at(t));
                        let pan = v.pan_lane.as_ref().map_or(v.pan, |l| l.value_at(t));
                        gains(vol, pan, song_volume, linear)
                    } else {
                        v.const_gain
                    };
                    let o = (f - block_start) as usize;
                    out_l[o] += sl * gl as f32;
                    out_r[o] += sr_ * gr as f32;
                }
                a.raw_pos += step;
            }
            v.end > block_end
        });

        // ── VST voices ─────────────────────────────────────────────────
        if !self.hosts.is_empty() {
            self.render_vsts(out_l, out_r, block_start);
        }

        // ── master bus effects ─────────────────────────────────────────
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

        self.pos = block_end;
    }

    /// Drive every installed VST host for this block.  Chunked because the
    /// shim was created with a fixed max block size; notes are dispatched
    /// with sample offsets so timing stays exact within the chunk.
    fn render_vsts(&mut self, out_l: &mut [f32], out_r: &mut [f32], block_start: i64) {
        let block_len = out_l.len() as i64;
        let mut chunk_start = block_start;
        let block_end = block_start + block_len;

        while chunk_start < block_end {
            let chunk_len =
            ((block_end - chunk_start) as usize).min(VST_BLOCK as usize);
            let chunk_end = chunk_start + chunk_len as i64;

            // Note-ons whose start falls in this chunk.
            while self
                .vst_pending
                .front()
                .map_or(false, |n| n.start < chunk_end)
                {
                    let n = self.vst_pending.pop_front().unwrap();
                    if let Some(host) = self.hosts.get_mut(&n.iid) {
                        let offset = (n.start - chunk_start).max(0) as i32;
                        host.note_on_at(n.note, n.velocity, offset);
                    }
                    self.vst_active.push(n);
                }

                // Note-offs for anything ending in this chunk.
                let mut i = 0;
                while i < self.vst_active.len() {
                    if self.vst_active[i].end <= chunk_end {
                        let n = self.vst_active.remove(i);
                        if let Some(host) = self.hosts.get_mut(&n.iid) {
                            let offset = (n.end - chunk_start).max(0) as i32;
                            host.note_off_at(n.note, offset);
                        }
                    } else {
                        i += 1;
                    }
                }

                // Zero scratch for this chunk.
                for s in self.vst_scratch_l[..chunk_len].iter_mut() {
                    *s = 0.0;
                }
                for s in self.vst_scratch_r[..chunk_len].iter_mut() {
                    *s = 0.0;
                }

                // Sum every host into scratch.
                let hosts = &mut self.hosts;
                let scratch_l = &mut self.vst_scratch_l;
                let scratch_r = &mut self.vst_scratch_r;
                for host in hosts.values_mut() {
                    host.process(
                        &mut scratch_l[..chunk_len],
                        &mut scratch_r[..chunk_len],
                    );
                }

                // Sum scratch into the output at the right offset.
                let out_off = (chunk_start - block_start) as usize;
                for j in 0..chunk_len {
                    out_l[out_off + j] += self.vst_scratch_l[j];
                    out_r[out_off + j] += self.vst_scratch_r[j];
                }

                chunk_start = chunk_end;
        }
    }
}
