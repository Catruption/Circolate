//! Braille waveform rasterisers.
//!
//! `rasterize_time_x` and `rasterize_time_y` are the shared primitives:
//! both take a `[(f32, f32)]` envelope, auto-scale it to fill `[-1, 1]`,
//! and return a `[h_dots][w_dots]` matrix of lit dots. The two differ
//! only in which axis time runs along:
//!
//! * `rasterize_time_x` — time on X, amplitude on Y. The vertical view
//!   and any standalone sample-preview widget use this.
//! * `rasterize_time_y` — time on Y, amplitude on X. The horizontal
//!   tracker uses this so waveforms sit *behind* the note text as
//!   horizontal bands per row.
//!
//! `render_envelope` is a convenience wrapper over `rasterize_time_x`
//! that packs dots into braille characters and applies styles. The
//! horizontal view can't use it directly because it needs to composite
//! waveform dots with note text on the same cell, so it calls
//! `rasterize_time_y` and does its own packing.

use ratatui::style::Style;
use ratatui::text::Span;

use crate::player::envelope::{synth_envelope_live, whole_sample_envelope};
use crate::player::Sample;

use super::braille::{braille_char, dot_bit};

const PLAYHEAD_GLYPH: char = '┃';
const AUTO_SCALE_MAX: f32 = 1000.0;
const AUTO_SCALE_FLOOR: f32 = 1e-5;

/// Normalize an envelope so its peak fills `[-1, 1]`.
///
/// A VST channel the picker attenuated to 0.058 gain has a raw
/// envelope peak around 0.001; without scaling it renders as a
/// sub-pixel strip. Skipped below `AUTO_SCALE_FLOOR` so a genuinely
/// silent channel stays flat instead of amplifying noise.
pub fn auto_scale(env: &mut [(f32, f32)]) {
    let mut peak = 0.0f32;
    for &(lo, hi) in env.iter() {
        let m = lo.abs().max(hi.abs());
        if m > peak {
            peak = m;
        }
    }
    if !peak.is_finite() || peak < AUTO_SCALE_FLOOR {
        return;
    }
    let scale = (1.0 / peak).min(AUTO_SCALE_MAX);
    if scale <= 1.05 {
        return;
    }
    for (lo, hi) in env.iter_mut() {
        *lo = (*lo * scale).clamp(-1.0, 1.0);
        *hi = (*hi * scale).clamp(-1.0, 1.0);
    }
}

/// Map a value interval `[lo, hi]` in `[-1, 1]` to a contiguous range
/// of dot rows `[d0, d1]` on a grid of `total_dots` rows. Handles the
/// sub-dot-width case (single sample, DC run) by snapping to the
/// nearest dot.
fn dot_range(lo: f32, hi: f32, total_dots: usize) -> (i64, i64) {
    if !lo.is_finite() || !hi.is_finite() || lo > hi {
        return (1, 0);
    }
    let t = total_dots.max(1) as f64;
    let lo = (lo as f64).clamp(-1.0, 1.0);
    let hi = (hi as f64).clamp(-1.0, 1.0);
    let top = (1.0 - hi) / 2.0;
    let bot = (1.0 - lo) / 2.0;
    let mut d0 = (top * t).ceil() as i64;
    let mut d1 = (bot * t).floor() as i64;
    if d0 > d1 {
        let mid = ((top + bot) * 0.5 * t).round() as i64;
        d0 = mid;
        d1 = mid;
    }
    let last = total_dots.max(1) as i64 - 1;
    (d0.clamp(0, last), d1.clamp(0, last))
}

/// Rasterize a min/max envelope into a dot matrix, time on X.
///
/// `env[k]` is the (lo, hi) pair for dot-column `k`. Return value is
/// `dots[y][x]`, `true` = lit. Envelope is auto-scaled first.
pub fn rasterize_time_x(
    env: &[(f32, f32)],
                        w_dots: usize,
                        h_dots: usize,
) -> Vec<Vec<bool>> {
    let mut dots = vec![vec![false; w_dots.max(1)]; h_dots.max(1)];
    if w_dots == 0 || h_dots == 0 {
        return dots;
    }
    let mut scaled = env.to_vec();
    auto_scale(&mut scaled);

    for x in 0..w_dots {
        let (lo, hi) = scaled.get(x).copied().unwrap_or((0.0, 0.0));
        let (d0, d1) = dot_range(lo, hi, h_dots);
        if d0 > d1 {
            continue;
        }
        let d0 = d0.max(0) as usize;
        let d1 = (d1 as usize).min(h_dots - 1);
        for d in d0..=d1 {
            dots[d][x] = true;
        }
    }
    dots
}

/// Rasterize a min/max envelope into a dot matrix, time on Y.
///
/// `env[y]` is the (lo, hi) pair for dot-row `y`. The amplitude range
/// `[lo, hi]` maps to a contiguous run of dot columns centred on the
/// middle of the matrix, so `lo = hi = 0` gives a single dot in the
/// centre and `lo = -1, hi = +1` fills the whole row. Envelope is
/// auto-scaled first.
pub fn rasterize_time_y(
    env: &[(f32, f32)],
                        w_dots: usize,
                        h_dots: usize,
) -> Vec<Vec<bool>> {
    let mut dots = vec![vec![false; w_dots.max(1)]; h_dots.max(1)];
    if w_dots == 0 || h_dots == 0 {
        return dots;
    }
    let mut scaled = env.to_vec();
    auto_scale(&mut scaled);

    let half = w_dots as f32 / 2.0;
    let last = (w_dots as isize) - 1;

    for y in 0..h_dots.min(scaled.len()) {
        let (lo, hi) = scaled[y];
        if !lo.is_finite() || !hi.is_finite() || lo > hi {
            continue;
        }
        let lo = lo.clamp(-1.0, 1.0);
        let hi = hi.clamp(-1.0, 1.0);
        // Clamp both ends to the valid pixel range. `lo = hi = +1.0`
        // used to produce `px_lo = px_hi = w_dots`, one past the last
        // valid column, so positive peaks at exactly full scale fell
        // off the edge and rendered as nothing. Clamping both ends
        // here means a full-scale peak draws on the last column
        // instead of skipping it.
        let px_lo = ((half + lo * half).round() as isize).clamp(0, last) as usize;
        let px_hi = ((half + hi * half).round() as isize).clamp(0, last) as usize;
        let (px_lo, px_hi) = if px_lo <= px_hi { (px_lo, px_hi) } else { (px_hi, px_lo) };
        for px in px_lo..=px_hi {
            dots[y][px] = true;
        }
    }
    dots
}

/// Pack a dot matrix into a `Vec<Vec<Span>>` of braille characters.
/// Playhead column, if any, is drawn as a solid bar over that column.
pub fn render_envelope(
    env: &[(f32, f32)],
                       w: usize,
                       h: usize,
                       base_style: Style,
                       playhead_col: Option<usize>,
                       playhead_style: Style,
) -> Vec<Vec<Span<'static>>> {
    let dots = rasterize_time_x(env, w * 2, h * 4);

    let mut out: Vec<Vec<Span<'static>>> = Vec::with_capacity(h);
    for y in 0..h {
        let mut row: Vec<Span<'static>> = Vec::with_capacity(w);
        for x in 0..w {
            if Some(x) == playhead_col {
                row.push(Span::styled(PLAYHEAD_GLYPH.to_string(), playhead_style));
                continue;
            }
            let mut bits = 0u8;
            for ly in 0..4 {
                let dy = y * 4 + ly;
                if dy >= h * 4 {
                    continue;
                }
                for lx in 0..2 {
                    let dx = x * 2 + lx;
                    if dx >= w * 2 {
                        continue;
                    }
                    if dots[dy][dx] {
                        bits |= dot_bit(lx, ly);
                    }
                }
            }
            let ch = if bits == 0 { ' ' } else { braille_char(bits) };
            row.push(Span::styled(ch.to_string(), base_style));
        }
        out.push(row);
    }
    out
}

pub fn render_waveform(
    sample: &Sample,
    w: usize,
    h: usize,
    base_style: Style,
    playhead_col: Option<usize>,
    playhead_style: Style,
) -> Vec<Vec<Span<'static>>> {
    let env = whole_sample_envelope(sample, w * 2);
    render_envelope(&env, w, h, base_style, playhead_col, playhead_style)
}

pub fn synth_waveform(
    snapshot: &[f32],
    w: usize,
    h: usize,
    base_style: Style,
    playhead_col: Option<usize>,
    playhead_style: Style,
) -> Vec<Vec<Span<'static>>> {
    let mut env = synth_envelope_live(snapshot, w * 2);
    if env.is_empty() {
        env = vec![(0.0, 0.0); w * 2];
    }
    render_envelope(&env, w, h, base_style, playhead_col, playhead_style)
}
