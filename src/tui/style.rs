//! Color mapping and braille helpers.

use ratatui::style::{Color, Modifier, Style};

use crate::sources::trackscript::hz_to_note;

/// Foreground for unplayed notes (waiting or future).
pub const UNPLAYED_FG: Color = Color::White;

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

pub fn pitch_hue(midi: i32) -> f64 {
    midi.rem_euclid(12) as f64 * 30.0
}

#[derive(Clone, Copy)]
pub enum NoteState {
    /// Currently sounding on the visible line.
    Playing,
    /// Already finished — either its body has passed on the current
    /// line, or it was carried over from an earlier line and is now
    /// only a fading tail.
    Played,
    /// Not yet reached on the current line.
    Waiting,
    /// On a later line; shown here because the column is in the layout,
    /// but the note hasn't been approached by the playhead yet.
    Future,
}

pub fn note_style(pitch_hz: f64, vol: f64, state: NoteState) -> Style {
    let midi = hz_to_note(pitch_hz);
    let hue = pitch_hue(midi);
    let sat = (vol / 100.0).clamp(0.0, 1.0);
    match state {
        NoteState::Playing => {
            // Currently sounding: pitch-colored background, white text.
            // We deliberately do NOT set fg to black here — the pitch
            // hue already provides the "this note is sounding" signal,
            // and black text is unreadable against low-saturation
            // (quiet) notes, since saturation tracks volume.
            let (r, g, b) = hsv_to_rgb(hue, sat, 1.0);
            Style::default()
            .fg(Color::White)
            .bg(Color::Rgb(r, g, b))
            .add_modifier(Modifier::BOLD)
        }
        NoteState::Played => {
            // Already finished.  Grey fg, no bg — the mosaic pixel
            // behind the glyph still comes through.
            Style::default().fg(Color::DarkGray)
        }
        NoteState::Waiting | NoteState::Future => {
            // The pattern itself.  Full-brightness single color, no bg —
            // mosaic pixel visible behind the glyph.
            Style::default()
            .fg(UNPLAYED_FG)
            .add_modifier(Modifier::BOLD)
        }
    }
}

#[inline]
pub fn dot_bit(lx: usize, ly: usize) -> u8 {
    match (lx, ly) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        (0, 3) => 0x40,
        (1, 3) => 0x80,
        _ => 0,
    }
}

pub fn braille_char(bits: u8) -> char {
    if bits == 0 {
        ' '
    } else {
        char::from_u32(0x2800 + bits as u32).unwrap_or(' ')
    }
}
