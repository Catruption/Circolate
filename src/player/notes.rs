//! Note model.  A trackscript event stream is parsed into `NoteRow`s
//! (note bodies with attached waypoints), then projected into
//! `DisplayNote`s for a particular view's window.
//!
//! This module is presentation-agnostic: it does not know about
//! ratatui, colours, or note states.  Formatting for the tracker
//! views lives in `crate::tui::shared::note_fmt`.

use crate::sources::trackscript::TrackscriptEvent;

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
    pub fine_pitch_delta: Option<i16>,
    pub release_delta: Option<i16>,
    pub mod_x_delta: Option<i16>,
    pub mod_y_delta: Option<i16>,
}

#[derive(Clone)]
pub struct DisplayNote {
    pub note: NoteRow,
    pub subs: u32,
    pub display_start: f64,
    pub display_end: f64,
    pub value_offset: f64,
    pub source_name: String,
}

pub enum ColumnLine {
    Note(DisplayNote),
    Divider { label: String },
}

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
