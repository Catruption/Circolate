//! Voice scheduling, mixer, and DSP primitives.

use std::collections::VecDeque;
use std::f64::consts::{FRAC_PI_4, PI};
use std::sync::{Arc, OnceLock};

use crate::sources::trackscript::C5_HZ;
use crate::timing::AutomationLane;

use super::project::{Project, Sample};

// ── scheduled voice ────────────────────────────────────────────────────

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
    Batch { epoch: u64, voices: Vec<Scheduled> },
    Seek { epoch: u64, pos: i64, voices: Vec<Scheduled> },
}

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
        }
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

    pub(crate) fn apply_seek(&mut self, to: i64, voices: Vec<Scheduled>) {
        self.pos = to.clamp(0, self.total);
        self.active.clear();
        self.pending.clear();
        self.ready = true;
        self.ingest(voices);
    }

    pub(crate) fn handle(&mut self, m: Msg) {
        match m {
            Msg::Batch { epoch, voices } => {
                if epoch == self.epoch {
                    self.ingest(voices);
                }
            }
            Msg::Seek { epoch, pos, voices } => {
                self.epoch = epoch;
                self.apply_seek(pos, voices);
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

        self.pos = block_end;
    }
}
