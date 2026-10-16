//! Textual formatting of a `NoteRow` for the tracker.  These functions
//! are the source of the `<§1/1,#C5,~1>` syntax the vertical view
//! prints and the horizontal view lays out in columns.

use crate::player::notes::{value_at, NoteRow};
use crate::formats::trackscript::hz_to_note;

use super::note_style::NoteState;

pub fn fmt_pitch(hz: f64) -> String {
    let midi = hz_to_note(hz);
    let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let oct = midi.div_euclid(12);
    format!("{}{}", names[midi.rem_euclid(12) as usize], oct)
}

pub fn fmt_num(v: f64) -> String {
    let s = format!("{:.2}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".into()
    } else {
        s.to_string()
    }
}

/// `fmt_note` reads `state` to decide whether the note's "remaining
/// duration" is shown as a live countdown (Playing) or frozen at its
/// end value (Played).  Values that ramp (pitch, volume, pan) are
/// sampled at `row` while playing and at `end_row` once done, so a
/// slide's destination and a ramp's final volume stay visible.
pub fn fmt_note(n: &NoteRow, row: f64, subs: u32, state: NoteState) -> String {
    let subs_f = subs.max(1) as f64;
    let beat = (n.start_row / subs_f).floor() + 1.0;
    let sub = (n.start_row % subs_f) + 1.0;

    let value_row = match state {
        NoteState::Played => n.end_row,
        _ => row,
    };

    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("§{}/{}", beat as i64, fmt_num(sub)));
    if let Some(hz) = value_at(&n.pitch_points, value_row) {
        parts.push(format!("#{}", fmt_pitch(hz)));
    }
    if let Some(d) = n.dur_beats {
        let shown = match state {
            NoteState::Played => 0.0,
            NoteState::Playing => {
                let elapsed_beats = ((row - n.start_row) / subs_f).max(0.0);
                (d - elapsed_beats).max(0.0)
            }
            NoteState::Waiting | NoteState::Future => d,
        };
        parts.push(format!("~{}", fmt_num(shown)));
    }
    if let Some(v) = value_at(&n.vol_points, value_row) {
        parts.push(format!("%{}", fmt_num(v)));
    }
    if let Some(p) = value_at(&n.pan_points, value_row) {
        if p.abs() > 0.01 {
            parts.push(format!("¶{}", fmt_num(p)));
        }
    }
    if let Some(d) = n.fine_pitch_delta {
        parts.push(format!("¢{:+}", d));
    }
    if let Some(d) = n.release_delta {
        parts.push(format!("₨{:+}", d));
    }
    if let Some(d) = n.mod_x_delta {
        parts.push(format!("₥{:+}", d));
    }
    if let Some(d) = n.mod_y_delta {
        parts.push(format!("¥{:+}", d));
    }
    format!("<{}>", parts.join(","))
}
