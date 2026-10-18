//! Envelope handling: point list → baked gain curve (amp) and SF2 ADSR (debug / filter).
//!
//! Slicex envelopes are one-shot and *linear in amplitude*.  SF2 volume
//! envelopes are linear in dB and wait for note-off, so they cannot
//! reproduce the shape.  The amp envelope is therefore evaluated with
//! `gain_at` and baked into each slice's PCM (see sf2.rs).
//!
//! Point format: points[0].dx is the start offset; for i > 0, dx is the
//! length of segment i-1 → i, and `curve` on point i bends that segment.
//! Time unit: 1.0 = one envelope time scale (the slice length by default,
//! or `SLICEX_ENV_SCALE` seconds).
//!
//! Calibration knobs (env vars):
//!   SLICEX_ENV_CURVE_K   curve law strength, default 2.0.  segment shape
//!                        is x^(2^(K*curve)).  0 = straight lines.
//!                        The law itself is a GUESS: compare against an
//!                        FL render of a single known envelope.
//!   SLICEX_ENV_CURVE_WEIGHT  only affects the ADSR debug decomposition.
//!
//! ## Provenance
//!
//! SRC[loc=envelope records (trest.rs / slicex.rs Envelope) decomp=TODO conf=verified for the layout, guess for the meaning]
//!
//! The point layout (f64 dx/x, f64 y, f32 curve, 4 more bytes) is verified: the
//! same record parses cleanly in slicex.rs.  How the values are USED here (x of
//! point i > 0 is the segment length, y is linear amplitude, time unit = slice
//! length, curve law, SPEED 0.5 = normal) comes from comparing against FL
//! renders, NOT from the DLL, so treat every function below as a model of the
//! plugin until the evaluator is found.
//!
//! To find it in the DLL: the reader checks version == 3 and loops with stride
//! 0x18; the evaluator is the function that takes that point array and a time and
//! calls pow / exp2 / interpolation.  Its curve law replaces `shape`, its time
//! unit replaces SLICEX_ENV_SCALE, and its SPEED mapping replaces `speed_mult`.

use super::trest::EnvelopePoint;

#[derive(Debug, Clone, Copy)]
pub struct AmpEnvelope {
    pub delay_ms: f64,
    pub attack_ms: f64,
    pub hold_ms: f64,
    pub decay_ms: f64,
    pub sustain: f64,
    pub release_ms: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct FilterEnvelope {
    pub delay_ms: f64,
    pub attack_ms: f64,
    pub hold_ms: f64,
    pub decay_ms: f64,
    pub sustain: f64,
    pub release_ms: f64,
    pub depth_cents: i16,
}

fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name).ok().and_then(|s| s.trim().parse::<f64>().ok())
}

fn curve_weight() -> f64 {
    env_f64("SLICEX_ENV_CURVE_WEIGHT")
        .filter(|w| (0.0..=1.0).contains(w))
        .unwrap_or(0.25)
}

pub fn curve_k() -> f64 {
    env_f64("SLICEX_ENV_CURVE_K").unwrap_or(2.0)
}

/// Segment shape, x in 0..1 → 0..1.  PLACEHOLDER law, see module docs.
// SRC[loc=EnvelopePoint.curve (f32 at point+16) decomp=TODO conf=guess]  x^(2^(K*c)) is a placeholder law.
fn shape(c: f64, k: f64, x: f64) -> f64 {
    if c == 0.0 || k == 0.0 {
        x
    } else {
        x.clamp(0.0, 1.0).powf(2f64.powf(k * c))
    }
}

/// Linear-amplitude envelope value at time `t` (in envelope time units).
// SRC[loc=envelope points decomp=TODO conf=guess]  segment-length semantics of dx and linear y are inferred.
pub fn gain_at(points: &[EnvelopePoint], t: f64, k: f64) -> f64 {
    let Some(first) = points.first() else { return 1.0 };
    let mut t0 = first.dx;
    if t <= t0 {
        return first.y;
    }
    for w in points.windows(2) {
        let p = &w[1];
        let t1 = t0 + p.dx;
        if t < t1 {
            let x = if p.dx > 0.0 { (t - t0) / p.dx } else { 1.0 };
            return w[0].y + (p.y - w[0].y) * shape(p.curve as f64, k, x);
        }
        t0 = t1;
    }
    points[points.len() - 1].y
}

/// SPEED envelope value (0..1, 0.5 = normal) -> playback-rate multiplier.
/// `octaves` is the pitch shift at the extremes (y=0 -> -octaves, y=1 ->
/// +octaves).  The real range is UNKNOWN - calibrate with
/// SLICEX_SPEED_OCTAVES against FL.
// SRC[loc=SPEED envelope, tail target 4 decomp=TODO conf=guess]  0.5 = normal speed and the +-octave range are unverified.
pub fn speed_mult(y: f64, octaves: f64) -> f64 {
    2f64.powf((y - 0.5) * 2.0 * octaves)
}

// SRC[loc=n/a decomp=n/a conf=guess]  debug ADSR view only; the audio uses gain_at.
impl AmpEnvelope {
    /// ADSR decomposition.  Debug / instrument-text only: the audio uses
    /// `gain_at`, not these numbers.
    pub fn from_points(points: &[EnvelopePoint], slice_secs: f64) -> Option<Self> {
        if points.is_empty() {
            return None;
        }
        let time_scale = slice_secs.max(0.001);
        let cw = curve_weight();

        let mut times = Vec::with_capacity(points.len());
        let mut t = 0.0f64;
        for (i, p) in points.iter().enumerate() {
            if i == 0 {
                t = p.dx;
            } else {
                t += p.dx * (1.0 + cw * (p.curve as f64));
            }
            times.push(t * time_scale);
        }

        let peak = points.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
        if !peak.is_finite() || peak <= 0.001 {
            return None;
        }
        let first_peak = points.iter().position(|p| (p.y - peak).abs() < 0.001).unwrap_or(0);
        let last_peak = points
            .iter()
            .rposition(|p| (p.y - peak).abs() < 0.001)
            .unwrap_or(first_peak);

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
        let peak = points.iter().map(|p| p.y).fold(0.0f64, f64::max);
        let last_peak = points.iter().rposition(|p| (p.y - peak).abs() < 0.001).unwrap_or(0);
        Some(FilterEnvelope {
            delay_ms: base.delay_ms,
            attack_ms: base.attack_ms,
            hold_ms: base.hold_ms,
            decay_ms: base.decay_ms,
            sustain: points[last_peak].y.clamp(0.0, 1.0),
            release_ms: base.release_ms,
            depth_cents,
        })
    }
}

pub fn ms_to_timecents(ms: f64) -> u16 {
    if ms <= 0.0 {
        return 0;
    }
    let tc = 1200.0 * (ms / 1000.0).log2();
    tc.clamp(-12000.0, 8000.0) as i16 as u16
}
