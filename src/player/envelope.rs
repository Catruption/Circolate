//! Waveform envelopes for the tracker's braille overlay.  Sample parts
//! come from the source PCM; VST parts come either from the live scope
//! ring buffer (preferred) or a phase-accumulator synth as fallback.

use crate::player::{Project, Sample};
use crate::player::notes::{split_notes, value_at};
use crate::formats::trackscript::C5_HZ;

/// Cap source samples read per pixel row.  Long samples otherwise stall
/// the renderer at 20 FPS.
const MAX_SLICE: usize = 512;

pub fn wrapped_pos(raw: f64, loop_pts: Option<(i64, i64)>) -> f64 {
    if let Some((ls, le)) = loop_pts {
        let ls_f = ls as f64;
        let le_f = le as f64;
        if le_f > ls_f && raw >= le_f {
            return ls_f + (raw - le_f) % (le_f - ls_f);
        }
    }
    raw
}

fn slice_minmax(
    sample: &Sample,
    s0: f64,
    s1: f64,
    loop_pts: Option<(i64, i64)>,
) -> (f32, f32) {
    let n = sample.left.len();
    if n == 0 {
        return (0.0, 0.0);
    }

    let span = (s1 - s0).abs();
    if span < 1.0 {
        let i = wrapped_pos(s0, loop_pts).round() as i64;
        let i = i.clamp(0, n as i64 - 1) as usize;
        let l = sample.left[i];
        let r = if sample.stereo { sample.right[i] } else { l };
        let m = (l + r) * 0.5;
        return (m, m);
    }

    let step = (span / MAX_SLICE as f64).max(1.0);
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    let mut p = s0.min(s1);
    let end = s0.max(s1);
    while p < end {
        let i = wrapped_pos(p, loop_pts).round() as i64;
        let i = i.clamp(0, n as i64 - 1) as usize;
        let l = sample.left[i];
        let r = if sample.stereo { sample.right[i] } else { l };
        let m = (l + r) * 0.5;
        if m < lo {
            lo = m;
        }
        if m > hi {
            hi = m;
        }
        p += step;
    }
    if !lo.is_finite() || !hi.is_finite() {
        (0.0, 0.0)
    } else {
        (lo, hi)
    }
}

pub fn whole_sample_envelope(sample: &Sample, pixel_rows: usize) -> Vec<(f32, f32)> {
    let n = sample.left.len();
    if n == 0 || pixel_rows == 0 {
        return vec![(0.0, 0.0); pixel_rows];
    }
    let nf = n as f64;
    let mut out = Vec::with_capacity(pixel_rows);
    for py in 0..pixel_rows {
        let s0 = (py as f64 / pixel_rows as f64) * nf;
        let s1 = ((py + 1) as f64 / pixel_rows as f64) * nf;
        out.push(slice_minmax(sample, s0, s1, None));
    }
    out
}

/// Scope samples shown per envelope bin.  The ring buffer holds
/// `SCOPE_LEN` (4096) samples, which is far too many to squeeze into a
/// column a few dozen rows tall — dozens of unsynchronised cycles
/// alias into noise.  Showing only the newest `bins * this` samples
/// zooms in to a handful of readable cycles.  Raise it to zoom out.
pub const SCOPE_SAMPLES_PER_BIN: usize = 4;

/// Min/max envelope of the live scope snapshot in `bins` bins.
///
/// Three things make this readable, compared with plain binning:
/// * only the newest `bins * SCOPE_SAMPLES_PER_BIN` samples are used;
/// * the start is snapped to a rising zero crossing, so a steady note
///   holds roughly still instead of scrolling randomly each frame;
/// * every bin also includes the first sample of the next bin, so
///   adjacent bins always touch and steep edges draw as a connected
///   line rather than isolated dots.
pub fn synth_envelope_live(snapshot: &[f32], bins: usize) -> Vec<(f32, f32)> {
    if bins == 0 || snapshot.is_empty() {
        return Vec::new();
    }
    let n = snapshot.len();
    let w = n.min(bins.saturating_mul(SCOPE_SAMPLES_PER_BIN)).max(1);
    let slack = n - w;

    // Trigger: look for a rising zero crossing in the older part of the
    // buffer; fall back to the newest window if there isn't one.
    let mut start = slack;
    let search_from = slack.saturating_sub(w.min(slack)).max(1);
    for i in search_from..slack {
        if snapshot[i - 1] <= 0.0 && snapshot[i] > 0.0 {
            start = i;
            break;
        }
    }

    let mut out = Vec::with_capacity(bins);
    for k in 0..bins {
        let a = (start + (k * w) / bins).min(n - 1);
        let b = (start + ((k + 1) * w) / bins).max(a + 1);
        let e = (b + 1).min(n);
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for &s in &snapshot[a..e] {
            if !s.is_finite() {
                continue;
            }
            if s < lo {
                lo = s;
            }
            if s > hi {
                hi = s;
            }
        }
        if lo.is_finite() && hi.is_finite() {
            out.push((lo, hi));
        } else {
            out.push((0.0, 0.0));
        }
    }
    out
}

pub fn synth_envelope_static(
    p: &Project,
    part_name: &str,
    pixel_rows: usize,
) -> Vec<(f32, f32)> {
    let subs = p.part_subs_per_beat(part_name);
    let rpmeasure = p.rows_per_measure();
    let notes = split_notes(
        p.part_events(part_name),
                            subs,
                            p.part_sample_offset_u32(part_name),
    );

    let mut out = Vec::with_capacity(pixel_rows);
    let mut phase = 0.0f64;

    for py in 0..pixel_rows {
        let t = py as f64 / pixel_rows as f64;
        let row = t * rpmeasure;

        let active = notes
        .iter()
        .rev()
        .find(|n| n.start_row <= row && row < n.end_row);
        let Some(note) = active else {
            out.push((0.0, 0.0));
            continue;
        };

        let pitch = value_at(&note.pitch_points, row).unwrap_or(C5_HZ);
        let vol = (value_at(&note.vol_points, row).unwrap_or(100.0) / 100.0) as f32;

        let cycles_total = (pitch / C5_HZ) * 2.0;
        phase += cycles_total / pixel_rows as f64;
        let s = (phase * std::f64::consts::TAU).sin() as f32 * vol;
        out.push((s, s));
    }
    out
}
