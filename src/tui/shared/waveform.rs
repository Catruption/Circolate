//! Braille waveform rasterisers.  `render_envelope` draws a min/max
//! envelope scaled to a rectangle; `render_waveform` computes the
//! envelope from a source `Sample` first; `synth_waveform` does the
//! same for a live scope snapshot.  All three return one `Vec<Span>`
//! per terminal row, ready to splat into a `Line`.
//!
//! The playhead column, if any, is rendered as a hard vertical bar that
//! replaces whatever would otherwise be in that cell — including blank
//! cells — so the marker stays visible over rests and quiet samples.

use ratatui::style::Style;
use ratatui::text::Span;

use crate::player::envelope::whole_sample_envelope;
use crate::player::Sample;

use super::braille::{braille_char, dot_bit};

/// Glyph used for the playhead column.  A heavy vertical bar reads as
/// "we are here" through any waveform, including silence.
const PLAYHEAD_GLYPH: char = '┃';

/// Rasterise a pre-computed min/max envelope.
pub fn render_envelope(
    env: &[(f32, f32)],
    w: usize,
    h: usize,
    base_style: Style,
    playhead_col: Option<usize>,
    playhead_style: Style,
) -> Vec<Vec<Span<'static>>> {
    let total_dots = (4 * h).max(1);

    let mut out: Vec<Vec<Span<'static>>> = Vec::with_capacity(h);
    for y in 0..h {
        let mut row: Vec<Span<'static>> = Vec::with_capacity(w);
        for x in 0..w {
            if Some(x) == playhead_col {
                row.push(Span::styled(PLAYHEAD_GLYPH.to_string(), playhead_style));
                continue;
            }
            let i0 = 2 * x;
            let i1 = 2 * x + 1;
            let (lo0, hi0) = env.get(i0).copied().unwrap_or((0.0, 0.0));
            let (lo1, hi1) = env.get(i1).copied().unwrap_or((0.0, 0.0));

            let lit = |lo: f32, hi: f32, frac: f64| -> bool {
                let top = (1.0 - hi as f64) / 2.0;
                let bot = (1.0 - lo as f64) / 2.0;
                frac >= top && frac <= bot
            };

            let mut bits = 0u8;
            for py in 0..4 {
                let dot = y * 4 + py;
                let frac = dot as f64 / total_dots as f64;
                if lit(lo0, hi0, frac) { bits |= dot_bit(0, py); }
                if lit(lo1, hi1, frac) { bits |= dot_bit(1, py); }
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
    let n = snapshot.len();
    if n == 0 {
        return vec![vec![Span::styled(" ".to_string(), base_style); w]; h];
    }
    let px_w = w * 2;
    let total_dots = (4 * h).max(1);

    let mut out: Vec<Vec<Span<'static>>> = Vec::with_capacity(h);
    for y in 0..h {
        let mut row: Vec<Span<'static>> = Vec::with_capacity(w);
        for x in 0..w {
            if Some(x) == playhead_col {
                row.push(Span::styled(PLAYHEAD_GLYPH.to_string(), playhead_style));
                continue;
            }
            let mut bits = 0u8;
            for px in [2 * x, 2 * x + 1] {
                let a = ((px as f64 / px_w as f64) * n as f64) as usize;
                let b = ((((px + 1) as f64) / px_w as f64) * n as f64) as usize;
                let a = a.min(n.saturating_sub(1));
                let b = b.max(a + 1).min(n);
                let mut lo = f32::INFINITY;
                let mut hi = f32::NEG_INFINITY;
                for &s in &snapshot[a..b] {
                    if s < lo { lo = s; }
                    if s > hi { hi = s; }
                }
                if !lo.is_finite() { continue; }
                let top = (1.0 - hi as f64) / 2.0;
                let bot = (1.0 - lo as f64) / 2.0;
                for py in 0..4 {
                    let dot = y * 4 + py;
                    let frac = dot as f64 / total_dots as f64;
                    if frac >= top && frac <= bot {
                        let bit = if px == 2 * x { 0 } else { 1 };
                        bits |= dot_bit(bit, py);
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