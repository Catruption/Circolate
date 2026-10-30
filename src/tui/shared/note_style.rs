//! Note-state classification and styling.
//!
//! Fixed state colors come from the theme (`Token::NotePlayingFg`,
//! `Token::NotePlayingBg`, `Token::Played`, `Token::Unplayed`).  The
//! pitch-driven background of a *playing* note is "synesthesia" —
//! procedural, tunable via `SynesthesiaConfig`, and when disabled falls
//! back to `Token::NotePlayingBg`.

use ratatui::style::{Color, Modifier, Style};

use crate::tui::config::SynesthesiaConfig;
use crate::tui::theme::{Theme, Token};

pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0).clamp(0.0, 255.0) as u8,
     ((g + m) * 255.0).clamp(0.0, 255.0) as u8,
     ((b + m) * 255.0).clamp(0.0, 255.0) as u8,
    )
}

#[derive(Clone, Copy)]
pub enum NoteState {
    Playing,
    Played,
    Waiting,
    Future,
}

/// The pitch-driven background for a playing note, or `None` if
/// synesthesia is disabled.  `pitch_hz` is in Hz; `vol` is 0..=100.
///
/// Returns `(Color, is_bright)`. `is_bright` tells the caller whether
/// to use black or white text for contrast.
fn synesthetic_bg(pitch_hz: f64, vol: f64, synth: &SynesthesiaConfig) -> Option<(Color, bool)> {
    if !synth.enabled {
        return None;
    }
    let midi = crate::formats::trackscript::hz_to_note(pitch_hz);
    let hue = midi.rem_euclid(12) as f64 * synth.hue_step;

    // Use volume to control brightness (value), not saturation.
    // This fades to black on silence, which fits a dark theme perfectly.
    let vol_ratio = (vol / 100.0).clamp(0.0, 1.0);
    let val = (0.1 + (vol_ratio * 0.9)) * synth.value;

    // Saturation now stays constant to keep the hue pure.
    let sat = synth.saturation_scale;

    let (r, g, b) = hsv_to_rgb(hue, sat, val);

    // If the background is bright, we need black text. If dark, white text.
    let is_bright = val > 0.6;

    Some((Color::Rgb(r, g, b), is_bright))
}

pub fn note_style(
    pitch_hz: f64,
    vol: f64,
    state: NoteState,
    theme: &Theme,
    synth: &SynesthesiaConfig,
    is_motion_highlight: bool, // NEW: X-Ray flag
) -> Style {
    match state {
        NoteState::Playing => {
            let (bg, is_bright) = synesthetic_bg(pitch_hz, vol, synth)
            .unwrap_or_else(|| (theme.color(Token::NotePlayingBg), false));

            // X-Ray: Massive leaps get Red text.
            // Otherwise, dynamic contrast (Black on bright, White on dark).
            let fg = if is_motion_highlight {
                Color::LightRed
            } else if is_bright {
                Color::Black
            } else {
                theme.color(Token::NotePlayingFg)
            };

            Style::default()
            .fg(fg)
            .bg(bg)
            .add_modifier(Modifier::BOLD)
        }
        NoteState::Played => Style::default().fg(theme.color(Token::Played)),
        NoteState::Waiting | NoteState::Future => Style::default()
        .fg(theme.color(Token::Unplayed))
        .add_modifier(Modifier::BOLD),
    }
}
