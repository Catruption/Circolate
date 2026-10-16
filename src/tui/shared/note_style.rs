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
fn synesthetic_bg(pitch_hz: f64, vol: f64, synth: &SynesthesiaConfig) -> Option<Color> {
    if !synth.enabled {
        return None;
    }
    let midi = crate::formats::trackscript::hz_to_note(pitch_hz);
    let hue = midi.rem_euclid(12) as f64 * synth.hue_step;
    let sat = if synth.saturation_from_volume {
        (vol / 100.0).clamp(0.0, 1.0) * synth.saturation_scale
    } else {
        synth.saturation_scale
    };
    let (r, g, b) = hsv_to_rgb(hue, sat, synth.value);
    Some(Color::Rgb(r, g, b))
}

pub fn note_style(
    pitch_hz: f64,
    vol: f64,
    state: NoteState,
    theme: &Theme,
    synth: &SynesthesiaConfig,
) -> Style {
    match state {
        NoteState::Playing => {
            let bg = synesthetic_bg(pitch_hz, vol, synth)
                .unwrap_or_else(|| theme.color(Token::NotePlayingBg));
            Style::default()
                .fg(theme.color(Token::NotePlayingFg))
                .bg(bg)
                .add_modifier(Modifier::BOLD)
        }
        NoteState::Played => Style::default().fg(theme.color(Token::Played)),
        NoteState::Waiting | NoteState::Future => Style::default()
            .fg(theme.color(Token::Unplayed))
            .add_modifier(Modifier::BOLD),
    }
}