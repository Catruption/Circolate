//! Pure DSP helpers used by the engine: pan law, sinc interpolation,
//! SF2-style volume envelope, and small buffer utilities.

use std::f64::consts::{FRAC_PI_4, PI};
use std::sync::OnceLock;

use super::Sample;

/// Replace NaN/inf with silence.
pub(super) fn sanitize(buf: &mut [f32]) {
    for s in buf {
        if !s.is_finite() {
            *s = 0.0;
        }
    }
}

/// `dst += src * gain`, element-wise.
pub(super) fn mix_add(dst_l: &mut [f32], dst_r: &mut [f32], src_l: &[f32], src_r: &[f32], gain: f32) {
    for (d, s) in dst_l.iter_mut().zip(src_l) {
        *d += *s * gain;
    }
    for (d, s) in dst_r.iter_mut().zip(src_r) {
        *d += *s * gain;
    }
}


#[inline]
pub(crate) fn gains(vol_pct: f64, pan: f64, song_volume: f64, linear: bool) -> (f64, f64) {
    let v = vol_pct * song_volume / 100.0;
    let pan = pan.clamp(-1.0, 1.0);
    if linear {
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
static SINC_UP32: OnceLock<[[f32; SINC_TAPS]; SINC_PHASES]> = OnceLock::new();

const _: () = assert!(SINC_TAPS == 8, "dot8 below is unrolled for 8 taps");

#[inline]
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = PI * x;
        px.sin() / px
    }
}

pub(super) fn up_table() -> &'static [[f64; SINC_TAPS]; SINC_PHASES] {
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

/// f32 copy of the sinc table, for the fast path.
pub(super) fn up_table_f32() -> &'static [[f32; SINC_TAPS]; SINC_PHASES] {
    SINC_UP32.get_or_init(|| {
        let src = up_table();
        let mut t = [[0.0f32; SINC_TAPS]; SINC_PHASES];
        for p in 0..SINC_PHASES {
            for k in 0..SINC_TAPS {
                t[p][k] = src[p][k] as f32;
            }
        }
        t
    })
}

/// Build every lookup table now, not mid-callback.
pub(super) fn init_tables() {
    up_table();
    up_table_f32();
}

/// 8-tap dot product as a balanced tree so the compiler can vectorize it.
#[inline(always)]
fn dot8(s: &[f32], w: &[f32; SINC_TAPS]) -> f32 {
    let s: &[f32; SINC_TAPS] = s.try_into().unwrap();
    ((s[0] * w[0] + s[4] * w[4]) + (s[1] * w[1] + s[5] * w[5]))
        + ((s[2] * w[2] + s[6] * w[6]) + (s[3] * w[3] + s[7] * w[7]))
}

/// Sinc interpolation.  Fast path when the whole 8-sample window lies
/// inside the sample and needs no loop wrapping (almost always);
/// otherwise falls back to the original per-tap wrap/clamp code.
pub(super) fn interpolate(smp: &Sample, pos: f64, _step: f64, raw_pos: f64) -> (f32, f32) {
    let n = smp.len() as i64;
    let center = pos.floor() as i64;
    let lo = center - SINC_TAPS_BEFORE;
    let hi = lo + SINC_TAPS as i64 - 1;

    let clean = lo >= 0
        && hi < n
        && match smp.loop_pts {
            Some((ls, le)) if le > ls => hi < le && !(raw_pos >= le as f64 && lo < ls),
            _ => true,
        };
    if clean {
        let frac = pos - center as f64;
        let phase = ((frac * SINC_PHASES as f64) as usize) & (SINC_PHASES - 1);
        let w = &up_table_f32()[phase];
        let lo = lo as usize;
        let l = dot8(&smp.left[lo..lo + SINC_TAPS], w);
        let r = if smp.stereo {
            dot8(&smp.right[lo..lo + SINC_TAPS], w)
        } else {
            l
        };
        return (l, r);
    }
    interpolate_slow(smp, pos, raw_pos)
}

fn interpolate_slow(smp: &Sample, pos: f64, raw_pos: f64) -> (f32, f32) {
    let n = smp.len() as i64;
    let center = pos.floor() as i64;

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
    (l as f32, r as f32)
}

// ── envelope phase boundaries ──────────────────────────────────────────

pub(crate) struct EnvState {
    pub(super) delay_sec: f64,
    pub(super) attack_end: f64,
    pub(super) hold_end: f64,
    pub(super) decay_end: f64,
    pub(super) release_start: f64,
    pub(super) release_end: f64,
    pub(super) peak: f32,
    pub(super) sustain: f32,
    pub(super) attack_inv: f32,
    pub(super) decay_inv: f32,
    pub(super) release_inv: f32,
    pub(super) start_sample: i64,
    /// Envelope level at `release_start`; filled by `with_release_level`.
    pub(super) release_level: f32,
}

impl EnvState {
    /// Precompute the level the release ramp starts from, so
    /// `env_level_at` doesn't recompute it for every release sample.
    pub(super) fn with_release_level(mut self) -> Self {
        self.release_level = env_sustain_level(&self, self.release_start);
        self
    }
}

#[inline]
pub(super) fn env_level_at(state: &EnvState, t_sec: f64) -> f32 {
    if t_sec < 0.0 {
        return 0.0;
    }
    if t_sec >= state.release_end {
        return 0.0;
    }
    if t_sec >= state.release_start {
        let at_release = state.release_level;
        let ft = ((t_sec - state.release_start) * state.release_inv as f64)
        .clamp(0.0, 1.0) as f32;
        return at_release * (1.0 - ft);
    }
    env_sustain_level(state, t_sec)
}

#[inline]
fn env_sustain_level(state: &EnvState, t_sec: f64) -> f32 {
    if t_sec < state.delay_sec {
        return 0.0;
    }
    let t_sec = t_sec - state.delay_sec;
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
