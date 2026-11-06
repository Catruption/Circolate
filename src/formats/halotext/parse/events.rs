//! Per-note event parsing.

use std::collections::HashMap;

use crate::formats::halotext::spec::{
    FIELD_DURATION, FIELD_FINE_PITCH, FIELD_LYRIC, FIELD_MOD_X, FIELD_MOD_Y,
    FIELD_PAN, FIELD_PITCH, FIELD_POSITION, FIELD_RELEASE, FIELD_SOURCE,
    FIELD_VOLUME, PART_HEADER_SIGIL,
};

use super::event_rx;
use super::parse_f;
use super::pitch::{parse_pitch_hz, parse_pitch_midi};
use super::source_rx;
use super::split_fields;
use crate::formats::halotext::model::HalotextEvent;

/// `∂` — Slicex envelope point.  Format: `∂value` or `∂value:tension:kind`.
const FIELD_ENV_POINT: char = '∂';

pub(crate) fn parse_part_events(
    body: &str,
    subs: u32,
    _beats_per_measure: u32,
    absolute_offset_rows: f64,
    source_offset: usize,
    source_line_starts: &[usize],
) -> Vec<HalotextEvent> {
    let event_rx = event_rx();
    let src_rx = source_rx();

    let trimmed = body.trim();
    let body_offset = body.len() - body.trim_start().len();
    let body = trimmed;
    let mut events = Vec::new();

    for em in event_rx.captures_iter(body) {
        let op = em.name("op").map(|m| m.as_str()).unwrap_or("").to_string();
        let inner = em.name("body").map(|m| m.as_str()).unwrap_or("");
        let match_start = em.get(0).map_or(0, |m| m.start());
        let match_end = em.get(0).map_or(match_start, |m| m.end());
        let source_start = source_offset + body_offset + match_start;
        let source_end = source_offset + body_offset + match_end;
        let source_line = source_line_at(source_line_starts, source_start);
        let source_line_start = source_line_starts
        .get(source_line.saturating_sub(1))
        .copied()
        .unwrap_or(source_start);

        let mut ev = HalotextEvent {
            op,
            row: 0.0,
            source_line,
            source_byte_column: source_start.saturating_sub(source_line_start),
            source_end_byte_column: source_end.saturating_sub(source_line_start),
            pitch_midi: None,
            pitch_hz: None,
            vol: None,
            pan: None,
            dur_beats: None,
            sample_override: None,
            offset: None,
            bank: None,
            preset: None,
            root_note: None,
            src_bpm: None,
            fine_pitch: None,
            release: None,
            mod_x: None,
            mod_y: None,
            lyric: None,
            raw: format!("<{}>", inner),
            waypoints: Vec::new(),
            env_value: None,
            env_tension: None,
            env_kind: None,
        };

        let mut beat: Option<f64> = None;
        let mut sub: Option<f64> = None;

        for f in split_fields(inner) {
            if let Some(rest) = f.strip_prefix(FIELD_POSITION) {
                beat = None;
                sub = None;
                if let Some((a, b)) = rest.split_once('/') {
                    if let (Some(ai), Some(bi)) = (parse_f(a), parse_f(b)) {
                        beat = Some(ai);
                        sub = Some(bi - 1.0);
                    }
                }
            } else if let Some(rest) = f.strip_prefix(FIELD_ENV_POINT) {
                // ∂value  |  ∂value:tension:kind
                let parts: Vec<&str> = rest.split(':').collect();
                ev.env_value   = parts.first().and_then(|x| x.trim().parse().ok());
                ev.env_tension = parts.get(1).and_then(|x| x.trim().parse().ok());
                ev.env_kind    = parts.get(2).and_then(|x| x.trim().parse().ok());
            } else if let Some(rest) = f.strip_prefix(PART_HEADER_SIGIL) {
                if !rest.contains('/') {
                    ev.src_bpm = parse_f(rest).filter(|b| *b > 0.0);
                }
            } else if let Some(rest) = f.strip_prefix(FIELD_PITCH) {
                ev.pitch_midi = parse_pitch_midi(rest);
                ev.pitch_hz = parse_pitch_hz(rest);
            } else if let Some(rest) = f.strip_prefix(FIELD_DURATION) {
                if let Some(v) = parse_f(rest.trim_start_matches('+')) {
                    ev.dur_beats = Some(v);
                }
            } else if let Some(rest) = f.strip_prefix(FIELD_VOLUME) {
                if let Some(v) = parse_f(rest.trim_start_matches('+')) {
                    ev.vol = Some(v);
                }
            } else if let Some(rest) = f.strip_prefix(FIELD_PAN) {
                if let Some(v) = parse_f(rest) {
                    ev.pan = Some(v);
                }
            } else if let Some(rest) = f.strip_prefix(FIELD_FINE_PITCH) {
                ev.fine_pitch = rest.trim().parse::<i16>().ok();
            } else if let Some(rest) = f.strip_prefix(FIELD_RELEASE) {
                ev.release = rest.trim().parse::<i16>().ok();
            } else if let Some(rest) = f.strip_prefix(FIELD_MOD_X) {
                ev.mod_x = rest.trim().parse::<i16>().ok();
            } else if let Some(rest) = f.strip_prefix(FIELD_MOD_Y) {
                ev.mod_y = rest.trim().parse::<i16>().ok();
            } else if let Some(rest) = f.strip_prefix(FIELD_LYRIC) {
                let trimmed = rest.trim().trim_matches(|c| c == '"' || c == '”' || c == '“' || c == '\'');
                if !trimmed.is_empty() {
                    ev.lyric = Some(trimmed.to_string());
                }
            } else if f.starts_with(FIELD_SOURCE) {
                if let Some(c) = src_rx.captures(&f) {
                    let filename = c[1].to_string();
                    ev.sample_override = Some(filename.clone());
                    let args_str = c.get(2).map(|m| m.as_str()).unwrap_or("");
                    let plus_str = c.get(3).map(|m| m.as_str()).unwrap_or("");
                    if !plus_str.is_empty() {
                        ev.offset = plus_str.parse().ok();
                    } else if !args_str.is_empty() {
                        let (pos, named) = parse_source_args(args_str);
                        let lower = filename.to_lowercase();
                        if lower.ends_with(".sf2") {
                            match pos.len() {
                                1 => {
                                    ev.preset = pos[0].parse().ok();
                                    ev.bank = Some(0);
                                }
                                2 => {
                                    ev.bank = pos[0].parse().ok();
                                    ev.preset = pos[1].parse().ok();
                                }
                                _ => {}
                            }
                        } else if lower.ends_with(".wav")
                            || lower.ends_with(".mp3")
                            || lower.ends_with(".ogg")
                            {
                                if let Some(s) = pos.first() {
                                    ev.offset = s.parse().ok();
                                }
                            }
                            if let Some(r) = named.get("root") {
                                ev.root_note = r.parse().ok();
                            }
                    }
                }
            }
        }

        let (Some(b), Some(s)) = (beat, sub) else { continue };
        ev.row = absolute_offset_rows + (b - 1.0) * subs as f64 + s;
        events.push(ev);
    }

    events
}

fn source_line_at(line_starts: &[usize], offset: usize) -> usize {
    let mut low = 0usize;
    let mut high = line_starts.len();
    while low < high {
        let mid = (low + high) / 2;
        if line_starts[mid] <= offset { low = mid + 1; } else { high = mid; }
    }
    low.max(1)
}

pub(crate) fn parse_source_args(args: &str) -> (Vec<String>, HashMap<String, String>) {
    let mut pos = Vec::new();
    let mut named = HashMap::new();
    for f in split_fields(args) {
        let f = f.trim();
        if f.is_empty() {
            continue;
        }
        if let Some((k, v)) = f.split_once(':') {
            named.insert(k.trim().to_string(), v.trim().to_string());
        } else {
            pos.push(f.to_string());
        }
    }
    (pos, named)
}
