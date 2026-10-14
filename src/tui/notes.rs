//! Note model and formatting.  A trackscript event stream is parsed into
//! `NoteRow`s (note bodies with attached waypoints), then projected into
//! `DisplayNote`s for the tracker's current window.

use crate::sources::trackscript::{hz_to_note, TrackscriptEvent};
use crate::tui::style::NoteState;

#[derive(Clone)]
pub struct Point {
    pub row: f64,
    pub value: f64,
    pub ramp_rows: f64,
}

#[derive(Clone)]
pub struct NoteRow {
    pub start_row: f64,
    pub end_row: f64,
    pub dur_beats: Option<f64>,
    pub offset: u32,
    pub pitch_points: Vec<Point>,
    pub vol_points: Vec<Point>,
    pub pan_points: Vec<Point>,
    /// Fine pitch offset in cents, as a signed delta from 0.
    pub fine_pitch_delta: Option<i16>,
    /// Release velocity, as a signed delta from the engine default of 64.
    pub release_delta: Option<i16>,
    /// Mod X, as a signed delta from the engine default of 128.
    pub mod_x_delta: Option<i16>,
    /// Mod Y, as a signed delta from the engine default of 128.
    pub mod_y_delta: Option<i16>,
}

#[derive(Clone)]
pub struct DisplayNote {
    pub note: NoteRow,
    pub subs: u32,
    pub display_start: f64,
    pub display_end: f64,
    pub value_offset: f64,
    /// The part name this note was contributed by.  Retained so the
    /// renderer can group carried notes by their source block.
    pub source_name: String,
}

/// A single row in a tracker column.  Either a note, or a labelled
/// divider inserted where the column's content comes from a different
/// block than the one above it.
pub enum ColumnLine {
    Note(DisplayNote),
    Divider { label: String },
}

/// Split a part's event stream into `NoteRow`s.
///
/// `default_offset` is the part's own `sample_offset` — the value a note
/// inherits when it has no per-note `(N)` offset of its own.  Before this
/// parameter existed, `split_notes` hardcoded the fallback to 0, so a
/// part whose offset lives on the definition (rather than on each note)
/// rendered its sample-position playhead starting at the top of the
/// sample regardless of what the audio engine was actually playing.
pub fn split_notes(
    events: &[TrackscriptEvent],
    subs: u32,
    default_offset: u32,
) -> Vec<NoteRow> {
    let subs_f = subs.max(1) as f64;
    let mut out: Vec<NoteRow> = Vec::new();
    let mut current: Option<usize> = None;

    for ev in events {
        if ev.is_waypoint() {
            if let Some(idx) = current {
                let n = &mut out[idx];
                let ramp = ev.dur_beats.unwrap_or(0.0) * subs_f;
                if let Some(hz) = ev.pitch_hz {
                    n.pitch_points.push(Point { row: ev.row, value: hz, ramp_rows: ramp });
                }
                if let Some(v) = ev.vol {
                    n.vol_points.push(Point { row: ev.row, value: v, ramp_rows: ramp });
                }
                if let Some(p) = ev.pan {
                    n.pan_points.push(Point { row: ev.row, value: p, ramp_rows: ramp });
                }
                let wp_end = ev.row + ramp;
                if wp_end > n.end_row {
                    n.end_row = wp_end;
                }
            }
        } else {
            let dur_beats = ev.dur_beats.filter(|d| *d > 0.0).unwrap_or(1.0);
            let end_row = ev.row + dur_beats * subs_f;
            let mut nr = NoteRow {
                start_row: ev.row,
                end_row,
                dur_beats: ev.dur_beats,
                // Inherit the part's offset when the note carries none.
                offset: ev.offset.unwrap_or(default_offset),
                pitch_points: Vec::new(),
                vol_points: Vec::new(),
                pan_points: Vec::new(),
                fine_pitch_delta: ev.fine_pitch,
                release_delta: ev.release,
                mod_x_delta: ev.mod_x,
                mod_y_delta: ev.mod_y,
            };
            if let Some(hz) = ev.pitch_hz {
                nr.pitch_points.push(Point { row: ev.row, value: hz, ramp_rows: 0.0 });
            }
            if let Some(v) = ev.vol {
                nr.vol_points.push(Point { row: ev.row, value: v, ramp_rows: 0.0 });
            }
            if let Some(p) = ev.pan {
                nr.pan_points.push(Point { row: ev.row, value: p, ramp_rows: 0.0 });
            }
            out.push(nr);
            current = Some(out.len() - 1);
        }
    }
    out
}

pub fn value_at(points: &[Point], row: f64) -> Option<f64> {
    let mut prev: Option<f64> = None;
    let mut cur: Option<&Point> = None;
    for p in points {
        if p.row <= row {
            if let Some(c) = cur {
                prev = Some(c.value);
            }
            cur = Some(p);
        } else {
            break;
        }
    }
    let p = cur?;
    if p.ramp_rows <= 0.0 || row >= p.row + p.ramp_rows {
        return Some(p.value);
    }
    let from = prev.unwrap_or(p.value);
    let frac = (row - p.row) / p.ramp_rows;
    Some(from + (p.value - from) * frac)
}

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

pub fn fmt_note(n: &NoteRow, row: f64, subs: u32, state: NoteState) -> String {
    let subs_f = subs.max(1) as f64;
    let beat = (n.start_row / subs_f).floor() + 1.0;
    let sub = (n.start_row % subs_f) + 1.0;

    // Once a note is done, freeze pitch/volume/pan at the values that
    // were in force at its end row — that picks up whatever the final
    // waypoint (or end-of-ramp interpolation) left them as, so a slide's
    // destination and a ramp's final volume stay visible.
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