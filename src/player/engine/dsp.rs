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

pub(super) fn interpolate(smp: &Sample, pos: f64, _step: f64, raw_pos: f64) -> (f32, f32) {
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

pub(super) struct EnvState {
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
        let at_release = env_sustain_level(state, state.release_start);
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
