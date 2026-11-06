//! Amp/Filter ADSR decomposition for the SF2 debug path.  The curve law
//! itself lives in `crate::formats::flp::slicex` and is re-exported here.

use super::trest::EnvelopePoint;

pub use crate::formats::flp::slicex::{speed_mult, shape_by_kind};

#[derive(Debug, Clone, Copy)]
pub struct AmpEnvelope {
    pub delay_ms: f64, pub attack_ms: f64, pub hold_ms: f64,
    pub decay_ms: f64, pub sustain: f64, pub release_ms: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct FilterEnvelope {
    pub delay_ms: f64, pub attack_ms: f64, pub hold_ms: f64,
    pub decay_ms: f64, pub sustain: f64, pub release_ms: f64,
    pub depth_cents: i16,
}

fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name).ok().and_then(|s| s.trim().parse::<f64>().ok())
}

fn curve_weight() -> f64 {
    env_f64("SLICEX_ENV_CURVE_WEIGHT").filter(|w| (0.0..=1.0).contains(w)).unwrap_or(0.25)
}

pub fn curve_k() -> f64 {
    env_f64("SLICEX_ENV_CURVE_K").unwrap_or(2.0)
}

/// Linear-amplitude envelope value at time `t` (envelope time units).
pub fn gain_at(points: &[EnvelopePoint], t: f64, _k: f64) -> f64 {
    let Some(first) = points.first() else { return 1.0 };
    let mut t0 = first.dx;
    if t <= t0 { return first.y; }
    for w in points.windows(2) {
        let p = &w[1];
        let t1 = t0 + p.dx;
        if t < t1 {
            let u = if p.dx > 0.0 { (t - t0) / p.dx } else { 1.0 };
            let s = shape_by_kind(p.kind(), p.curve as f64, u);
            return w[0].y + (p.y - w[0].y) * s;
        }
        t0 = t1;
    }
    points[points.len() - 1].y
}

impl AmpEnvelope {
    pub fn from_points(points: &[EnvelopePoint], slice_secs: f64) -> Option<Self> {
        if points.is_empty() { return None; }
        let time_scale = slice_secs.max(0.001);
        let cw = curve_weight();
        let mut times = Vec::with_capacity(points.len());
        let mut t = 0.0f64;
        for (i, p) in points.iter().enumerate() {
            if i == 0 { t = p.dx; }
            else { t += p.dx * (1.0 + cw * (p.curve as f64)); }
            times.push(t * time_scale);
        }
        let peak = points.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
        if !peak.is_finite() || peak <= 0.001 { return None; }
        let first_peak = points.iter().position(|p| (p.y - peak).abs() < 0.001).unwrap_or(0);
        let last_peak = points.iter().rposition(|p| (p.y - peak).abs() < 0.001).unwrap_or(first_peak);
        Some(AmpEnvelope {
            delay_ms: 0.0,
            attack_ms: (times[first_peak] - times[0]).max(0.0) * 1000.0,
            hold_ms: (times[last_peak] - times[first_peak]).max(0.0) * 1000.0,
            decay_ms: (*times.last()? - times[last_peak]).max(0.0) * 1000.0,
            sustain: 0.0,
            release_ms: 10.0,
        })
    }
}

impl FilterEnvelope {
    pub fn from_points(points: &[EnvelopePoint], slice_secs: f64, depth_cents: i16) -> Option<Self> {
        let base = AmpEnvelope::from_points(points, slice_secs)?;
        let sustain = points.last().map(|p| p.y.clamp(0.0, 1.0)).unwrap_or(0.0);
        Some(FilterEnvelope {
            delay_ms: base.delay_ms, attack_ms: base.attack_ms, hold_ms: base.hold_ms,
            decay_ms: base.decay_ms, sustain, release_ms: base.release_ms, depth_cents,
        })
    }
}

pub fn ms_to_timecents(ms: f64) -> u16 {
    if ms <= 0.0 { return (-32768i16) as u16; }
    let tc = 1200.0 * (ms / 1000.0).log2();
    tc.clamp(-12000.0, 8000.0) as i16 as u16
}
