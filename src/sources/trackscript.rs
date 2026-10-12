//! Trackscript parser.
//!
//! 1.11 syntax:
//!
//!   ƒNAME(<|N/D, args>)[ events ];    part definition
//!   ƒNAME(ƒA, ƒB);                    composite block
//!   ƒNAME();  ƒNAME(<args>);          call in BlockList
//!   @<§pos, %expr, ƒTarget>;          shim event in BlockList
//!
//! Args use the same field list as notes.  Fields a definition accepts
//! become its defaults for that field; a note that omits a field
//! inherits the default.
//!
//! Sections (`|N/D[...]`) are layout markers within a part.  A waypoint
//! chain crosses section boundaries; the header tells the TUI where to
//! draw a measure line, and tells the parser where a new coordinate
//! frame starts.

use anyhow::{anyhow, Result};
use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use crate::spec::{
    active_event_prefix_glyphs, FIELD_DURATION, FIELD_PAN, FIELD_PITCH, FIELD_POSITION,
    FIELD_SOURCE, FIELD_VOLUME, PART_HEADER_SIGIL,
};

pub const C5_HZ: f64 = 523.2511306;
#[allow(dead_code)]
pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

// ── data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct TrackscriptEvent {
    pub op: String,
    pub row: f64,
    pub pitch_midi: Option<i32>,
    pub pitch_hz: Option<f64>,
    pub vol: Option<f64>,
    pub pan: Option<f64>,
    pub dur_beats: Option<f64>,
    pub sample_override: Option<String>,
    pub offset: Option<u32>,
    /// Native tempo of the sample, from a `|bpm` field.  The player
    /// stretches the sample by song tempo / `src_bpm`.
    pub src_bpm: Option<f64>,
    pub raw: String,
}

impl TrackscriptEvent {
    pub fn is_waypoint(&self) -> bool {
        self.op == crate::spec::EV_WAYPOINT
    }
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct TrackscriptPart {
    pub name: String,
    pub sample_path: String,
    pub beats_per_measure: u32,
    pub subs_per_beat: u32,
    pub len_beats: Option<f64>,
    pub sample_offset: u32,
    /// Default native tempo of the part's sample (`|bpm` in the part args).
    pub src_bpm: Option<f64>,
    pub events: Vec<TrackscriptEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_vol: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_pan: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_dur: Option<f64>,
}

/// A shim event on a BlockList line.  Applies to that line's measure,
/// starting at `row` (local to the measure).
#[derive(Debug, Clone, Serialize)]
pub struct ShimEvent {
    pub line: usize,
    pub row: f64,
    pub target: String,
    pub property: String,
    pub start_value: f64,
    pub op: String,
    pub amount: f64,
    pub interval_rows: f64,
    pub count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct NoteEvent {
    pub row: f64,
    pub pitch: i32,
    pub pitch_hz: f64,
    pub voice: String,
    pub sample: Option<String>,
    pub duration_rows: Option<f64>,
    pub duration_beats: Option<f64>,
    pub section: usize,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct Config {
    pub bpm: f64,
    pub subdivisions: u32,
    pub time_signature: (u32, u32),
    pub song_volume: f64,
    pub panning_law: String,
    pub raw: HashMap<String, String>,
}

#[derive(Debug, Default, Serialize)]
#[allow(dead_code)]
pub struct TrackscriptFile {
    pub config: Config,
    pub tempo_events: Vec<(f64, f64, f64)>,
    pub block_list: Vec<Vec<String>>,
    pub shims: Vec<ShimEvent>,
    pub blocks: HashMap<String, Vec<String>>,
    pub parts: HashMap<String, TrackscriptPart>,
    pub notes: Vec<NoteEvent>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bpm: 120.0,
            subdivisions: 4,
            time_signature: (4, 4),
            song_volume: 100.0,
            panning_law: "equal_power".into(),
            raw: HashMap::new(),
        }
    }
}

// ── pitch ──────────────────────────────────────────────────────────────

pub fn note_to_hz(note: i32) -> f64 {
    C5_HZ * 2f64.powf((note as f64 - 60.0) / 12.0)
}

pub fn hz_to_note(hz: f64) -> i32 {
    (60.0 + 12.0 * (hz / C5_HZ).log2()).round() as i32
}

fn parse_note_name(text: &str) -> Option<i32> {
    let text = text.trim();
    let mut chars = text.chars();
    let first = chars.next()?.to_ascii_uppercase();
    let base = match first {
        'C' => 0, 'D' => 2, 'E' => 4, 'F' => 5,
        'G' => 7, 'A' => 9, 'B' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    let (accidental, octave_part) = if let Some(r) = rest.strip_prefix('#') {
        (1i32, r)
    } else if let Some(r) = rest.strip_prefix('b') {
        (-1i32, r)
    } else {
        (0i32, rest)
    };
    let digits = octave_part.strip_prefix('-').unwrap_or(octave_part);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let octave: i32 = octave_part.parse().ok()?;
    Some(octave * 12 + base + accidental)
}

pub fn parse_pitch_midi(text: &str) -> Option<i32> {
    if let Some(n) = parse_note_name(text) {
        return Some(n);
    }
    text.trim()
    .parse::<f64>()
    .ok()
    .filter(|h| *h > 0.0)
    .map(hz_to_note)
}

pub fn parse_pitch_hz(text: &str) -> Option<f64> {
    if let Some(n) = parse_note_name(text) {
        return Some(note_to_hz(n));
    }
    text.trim().parse::<f64>().ok().filter(|h| *h > 0.0)
}

// ── low-level text utilities ───────────────────────────────────────────

fn parse_f(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let bytes = line.as_bytes();
        let mut in_quote = false;
        let mut cut = line.len();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'\'' {
                in_quote = !in_quote;
            } else if !in_quote && c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                cut = i;
                break;
            }
            i += 1;
        }
        out.push_str(&line[..cut]);
    }
    out
}

fn match_pair(text: &str, start: usize, open_ch: u8, close_ch: u8) -> usize {
    let bytes = text.as_bytes();
    debug_assert_eq!(bytes[start], open_ch);
    let mut depth = 1i32;
    let mut i = start + 1;
    let mut in_quote = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\'' {
            in_quote = !in_quote;
        } else if !in_quote {
            if c == open_ch {
                depth += 1;
            } else if c == close_ch {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
        }
        i += 1;
    }
    bytes.len()
}

fn extract_block<'a>(text: &'a str, header_rx: &Regex) -> Option<&'a str> {
    let m = header_rx.find(text)?;
    let brace = m.end().checked_sub(1)?;
    if text.as_bytes().get(brace) != Some(&b'{') {
        return None;
    }
    let close = match_pair(text, brace, b'{', b'}');
    Some(&text[brace + 1..close])
}

fn split_fields(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in body.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                cur.push(c);
            }
            ',' => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn next_char_boundary(s: &str, i: usize) -> usize {
    i + s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)
}

// ── spec-driven regexes ────────────────────────────────────────────────

fn event_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        let glyphs = active_event_prefix_glyphs();
        let op = if glyphs.is_empty() {
            String::new()
        } else {
            let alts: Vec<String> = glyphs.into_iter().map(regex::escape).collect();
            format!("(?P<op>{})?", alts.join("|"))
        };
        Regex::new(&format!(r"{op}\s*<(?P<body>[^<>]*)>")).unwrap()
    })
}

fn source_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        let src = regex::escape(FIELD_SOURCE);
        Regex::new(&format!(r"^{src}'([^']+)'(?:\((\d*)\)|\+(\d+))?")).unwrap()
    })
}

fn call_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        let src = regex::escape(FIELD_SOURCE);
        Regex::new(&format!(r"{src}([A-Za-z_][A-Za-z0-9_]*)")).unwrap()
    })
}

fn part_header_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        let sigil = regex::escape(PART_HEADER_SIGIL);
        let dur = regex::escape(FIELD_DURATION);
        Regex::new(&format!(
            r"^\s*{sigil}(\d+)\s*/\s*(\d+)\s*(?:{dur}([\d.]+)\s*)?\["
        ))
        .unwrap()
    })
}

fn tempo_event_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        let wp = regex::escape(crate::spec::EV_WAYPOINT);
        Regex::new(&format!(r"{wp}?\s*<([^<>]+)>")).unwrap()
    })
}

// ── config / tempo ─────────────────────────────────────────────────────

fn parse_config(body: &str) -> Config {
    let mut cfg = Config::default();
    for entry in body.split(';') {
        let Some((key, value)) = entry.split_once(':') else { continue };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() {
            continue;
        }
        cfg.raw.insert(key.to_string(), value.to_string());
        match key {
            "BeatsPerMinute" => {
                if let Some(v) = parse_f(value) {
                    cfg.bpm = v;
                }
            }
            "Subdivisions" => {
                if let Ok(v) = value.parse() {
                    cfg.subdivisions = v;
                }
            }
            "SongVolume" => {
                if let Some(f) = parse_f(value.trim_end_matches('%')) {
                    cfg.song_volume = f;
                }
            }
            "TimeSignature" => {
                if let Some((a, b)) = value.split_once('/') {
                    if let (Ok(ai), Ok(bi)) = (a.trim().parse(), b.trim().parse()) {
                        cfg.time_signature = (ai, bi);
                    }
                }
            }
            "PanningLaw" => cfg.panning_law = value.trim().to_lowercase(),
            _ => {}
        }
    }
    cfg
}

fn parse_tempo(body: &str, subs: u32) -> Vec<(f64, f64, f64)> {
    let event_rx = tempo_event_rx();
    let body = body.trim();
    let measure_rows = 4.0 * subs as f64;
    let mut events = Vec::new();

    for em in event_rx.captures_iter(body) {
        let inner = em.get(1).unwrap();
        let mut row: Option<f64> = None;
        let mut bpm: Option<f64> = None;
        let mut ramp = 0.0f64;

        for f in inner.as_str().split(',').map(str::trim) {
            if let Some(rest) = f.strip_prefix(FIELD_POSITION) {
                row = None;
                if let Some((a, b)) = rest.split_once('/') {
                    if let (Some(ai), Some(bi)) = (parse_f(a), parse_f(b)) {
                        let local = (ai - 1.0) * subs as f64 + bi - 1.0;
                        let measure = body[..inner.start()].matches('\n').count() as f64;
                        row = Some(measure * measure_rows + local);
                    }
                }
            } else if let Some(rest) = f.strip_prefix(FIELD_PITCH) {
                if let Some(v) = parse_f(rest) {
                    bpm = Some(v);
                }
            } else if let Some(rest) = f.strip_prefix(FIELD_DURATION) {
                if let Some(v) = parse_f(rest.trim_start_matches('+')) {
                    ramp = v;
                }
            }
        }

        if let (Some(r), Some(b)) = (row, bpm) {
            if (r + b + ramp).is_finite() {
                events.push((r, b, ramp));
            }
        }
    }
    events
}

// ── shims ──────────────────────────────────────────────────────────────

fn parse_shim_event(inner: &str, line: usize, subs: u32) -> Option<ShimEvent> {
    let fields = split_fields(inner);
    let mut row = 0.0f64;
    let mut target = String::new();
    let mut property = String::new();
    let mut start_value = 0.0f64;
    let mut op = String::new();
    let mut amount = 0.0f64;
    let mut interval_rows = 0.0f64;
    let mut count: Option<u32> = None;

    for f in fields {
        if let Some(rest) = f.strip_prefix(FIELD_POSITION) {
            if let Some((a, b)) = rest.split_once('/') {
                if let (Some(ai), Some(bi)) = (parse_f(a), parse_f(b)) {
                    row = (ai - 1.0) * subs as f64 + (bi - 1.0);
                }
            }
        } else if let Some(rest) = f.strip_prefix(FIELD_SOURCE) {
            target = rest.trim().to_string();
        } else if let Some(sym) = value_field_symbol(&f) {
            // Try the full operator form first (`%100-0.8^0/2:8`).
            if let Some((s, val, o, amt, ir, cnt)) = parse_operator(&f, subs) {
                property = s;
                start_value = val;
                op = o;
                amount = amt;
                interval_rows = ir;
                count = cnt;
            } else {
                // Plain value: `#110`, `%100`, `¶0`.
                let rest = &f[sym.len()..];
                property = sym.to_string();
                start_value = parse_f(rest).unwrap_or(0.0);
            }
        }
    }

    if target.is_empty() {
        return None;
    }

    Some(ShimEvent {
        line,
         row,
         target,
         property,
         start_value,
         op,
         amount,
         interval_rows,
         count,
    })
}

/// Returns the leading field glyph if `f` starts with one of the
/// value-carrying symbols (`%`, `#`, `¶`).
fn value_field_symbol(f: &str) -> Option<&'static str> {
    for sym in [FIELD_VOLUME, FIELD_PITCH, FIELD_PAN] {
        if f.starts_with(sym) {
            return Some(sym);
        }
    }
    None
}

/// Parse `%100-0.8^0/2:8` into (symbol, value, op, amount, interval_rows, count).
fn parse_operator(
    field: &str,
    subs: u32,
) -> Option<(String, f64, String, f64, f64, Option<u32>)> {
    static OP_RX: OnceLock<Regex> = OnceLock::new();
    let rx = OP_RX.get_or_init(|| {
        Regex::new(
            r"^([%#¶])(-?\d+(?:\.\d+)?)([+\-*±])(\d+(?:\.\d+)?)\^(-?\d+(?:\.\d+)?)/(\d+(?:\.\d+)?)(?::(\d+))?$"
        ).unwrap()
    });
    let caps = rx.captures(field)?;
    let sym = caps[1].to_string();
    let val: f64 = caps[2].parse().ok()?;
    let op = caps[3].to_string();
    let amt: f64 = caps[4].parse().ok()?;
    let beat: f64 = caps[5].parse().ok()?;
    let sub: f64 = caps[6].parse().ok()?;
    let interval_rows = beat * subs as f64 + sub;
    let count: Option<u32> = caps.get(7).and_then(|m| m.as_str().parse().ok());

    Some((sym, val, op, amt, interval_rows, count))
}

// ── block list ─────────────────────────────────────────────────────────

fn parse_block_list(body: &str, subs: u32) -> (Vec<Vec<String>>, Vec<ShimEvent>) {
    let call_rx = call_rx();
    let mut lines = Vec::new();
    let mut shims = Vec::new();

    for raw in body.trim().split('\n') {
        if raw.trim().is_empty() {
            continue;
        }

        // Strip shims out of the line, recording them by index.
        let mut cleaned = String::with_capacity(raw.len());
        let mut rest = raw;
        loop {
            let Some(at_pos) = rest.find("@<") else {
                cleaned.push_str(rest);
                break;
            };
            cleaned.push_str(&rest[..at_pos]);
            cleaned.push(' ');
            let after_at = &rest[at_pos + 2..];
            let Some(close_rel) = after_at.find('>') else {
                // Malformed; drop the rest of the line's shim.
                break;
            };
            let inner = &after_at[..close_rel];
            if let Some(ev) = parse_shim_event(inner, lines.len(), subs) {
                shims.push(ev);
            }
            rest = &after_at[close_rel + 1..];
        }

        let names: Vec<String> = call_rx
        .captures_iter(&cleaned)
        .map(|c| c[1].to_string())
        .collect();

        lines.push(names);
    }

    (lines, shims)
}

// ── part events ────────────────────────────────────────────────────────

/// Parse a section body into events.  `absolute_offset_rows` is the row
/// at which this section begins within the part, accumulated across all
/// preceding sections at their own resolution.
fn parse_part_events(
    body: &str,
    subs: u32,
    _beats_per_measure: u32,
    absolute_offset_rows: f64,
) -> Vec<TrackscriptEvent> {
    let event_rx = event_rx();
    let src_rx = source_rx();

    let body = body.trim();
    let mut events = Vec::new();

    for em in event_rx.captures_iter(body) {
        let op = em.name("op").map(|m| m.as_str()).unwrap_or("").to_string();
        let inner = em.name("body").map(|m| m.as_str()).unwrap_or("");

        let mut ev = TrackscriptEvent {
            op,
            row: 0.0,
            pitch_midi: None,
            pitch_hz: None,
            vol: None,
            pan: None,
            dur_beats: None,
            sample_override: None,
            offset: None,
            src_bpm: None,
            raw: format!("<{}>", inner),
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
            } else if let Some(rest) = f.strip_prefix(PART_HEADER_SIGIL) {
                // `|135` is the sample's native BPM.  `|4/4` is a frame
                // and means nothing on a note.
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
            } else if f.starts_with(FIELD_SOURCE) {
                if let Some(c) = src_rx.captures(&f) {
                    ev.sample_override = Some(c[1].to_string());
                    let raw = c
                    .get(2)
                    .filter(|m| !m.as_str().is_empty())
                    .or_else(|| c.get(3))
                    .map(|m| m.as_str())
                    .unwrap_or("");
                    ev.offset = Some(if raw.is_empty() { 0 } else { raw.parse().unwrap_or(0) });
                }
            }
        }

        let (Some(b), Some(s)) = (beat, sub) else { continue };
        ev.row = absolute_offset_rows
        + (b - 1.0) * subs as f64
        + s;
        events.push(ev);
    }

    events
}

// ── block array ────────────────────────────────────────────────────────

type BlockMap = HashMap<String, Vec<String>>;
type PartMap = HashMap<String, TrackscriptPart>;

fn parse_block_array(body: &str) -> (BlockMap, PartMap) {
    let bytes = body.as_bytes();
    let src_rx = source_rx();
    let call_rx = call_rx();
    let mut blocks = BlockMap::new();
    let mut parts = PartMap::new();
    let mut pos = 0usize;

    while pos < bytes.len() {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }

        // Expect ƒ
        if !body[pos..].starts_with(FIELD_SOURCE) {
            pos = next_char_boundary(body, pos);
            continue;
        }
        let name_start = pos + FIELD_SOURCE.len();
        let name_end = body[name_start..]
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .map(|i| name_start + i)
        .unwrap_or(body.len());
        let name = body[name_start..name_end].to_string();
        if name.is_empty() {
            pos = next_char_boundary(body, pos);
            continue;
        }
        pos = name_end;
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }

        // Expect (
        if pos >= bytes.len() || bytes[pos] != b'(' {
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }
        let args_open = pos;
        let args_close = match_pair(body, args_open, b'(', b')');
        let args_str = body[args_open + 1..args_close].trim().to_string();
        pos = args_close + 1;
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }

        // ── composite: args don't start with `<` ──────────────────────
        if !args_str.starts_with('<') {
            let calls: Vec<String> = call_rx
            .captures_iter(&args_str)
            .map(|c| c[1].to_string())
            .collect();
            blocks.insert(name, calls);
            if pos < bytes.len() && bytes[pos] == b';' {
                pos += 1;
            }
            continue;
        }

        // ── part: args are `<|N/D, ƒ'path', %vol, ¶pan, ~dur>` ────────
        let inner = args_str
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();

        let mut sample_path = String::new();
        let mut sample_offset: u32 = 0;
        let mut src_bpm: Option<f64> = None;
        let mut default_vol: Option<f64> = None;
        let mut default_pan: Option<f64> = None;
        let mut default_dur: Option<f64> = None;
        let mut beats_per_measure: u32 = 4;
        let mut subs_per_beat: u32 = 4;
        let mut frame_from_args = false;

        for f in split_fields(inner) {
            if let Some(rest) = f.strip_prefix(PART_HEADER_SIGIL) {
                if let Some((b, s)) = rest.split_once('/') {
                    if let (Ok(bv), Ok(sv)) = (b.trim().parse(), s.trim().parse()) {
                        beats_per_measure = bv;
                        subs_per_beat = sv;
                        frame_from_args = true;
                    }
                } else {
                    // `|135`: the sample's native BPM.
                    src_bpm = parse_f(rest).filter(|b| *b > 0.0);
                }
            } else if f.starts_with(FIELD_SOURCE) {
                if let Some(c) = src_rx.captures(&f) {
                    sample_path = c[1].to_string();
                    sample_offset = c
                    .get(2)
                    .map(|m| m.as_str().parse().unwrap_or(0))
                    .unwrap_or(0);
                }
            } else if let Some(rest) = f.strip_prefix(FIELD_VOLUME) {
                default_vol = parse_f(rest.trim_start_matches('+'));
            } else if let Some(rest) = f.strip_prefix(FIELD_PAN) {
                default_pan = parse_f(rest);
            } else if let Some(rest) = f.strip_prefix(FIELD_DURATION) {
                default_dur = parse_f(rest.trim_start_matches('+'));
            }
        }

        // ── body ───────────────────────────────────────────────────────
        let mut all_events: Vec<TrackscriptEvent> = Vec::new();

        if pos < bytes.len() && bytes[pos] == b'[' {
            // Single section; frame came from the args.
            let open = pos;
            let close = match_pair(body, open, b'[', b']');
            let events_str = &body[open + 1..close];
            all_events.extend(parse_part_events(
                events_str,
                subs_per_beat,
                beats_per_measure,
                0.0,
            ));
            pos = close + 1;
        } else if pos < bytes.len() && bytes[pos] == PART_HEADER_SIGIL.as_bytes()[0] {
            // Multi-section body.  Each `|N/D[events]` has its own frame.
            // Sections accumulate row offsets so a waypoint chain crossing
            // a boundary lands on the correct absolute row within the part.
            let mut section_start_row: f64 = 0.0;

            while pos < bytes.len() && bytes[pos] == PART_HEADER_SIGIL.as_bytes()[0] {
                let hdr_start = pos + PART_HEADER_SIGIL.len();
                let Some(bracket_rel) = body[hdr_start..].find('[') else { break };
                let bracket_open = hdr_start + bracket_rel;
                let header = body[hdr_start..bracket_open].trim();

                let Some((nb_str, ns_str)) = header.split_once('/') else { break };
                let (Ok(nb), Ok(ns)) = (
                    nb_str.trim().parse::<u32>(),
                                        ns_str.trim().parse::<u32>(),
                ) else { break };

                if !frame_from_args {
                    beats_per_measure = nb;
                    subs_per_beat = ns;
                    frame_from_args = true;
                }

                let bracket_close = match_pair(body, bracket_open, b'[', b']');
                let events_str = &body[bracket_open + 1..bracket_close];
                all_events.extend(parse_part_events(
                    events_str,
                    ns,
                    nb,
                    section_start_row,
                ));

                // One section = one measure.  Advance by its own length
                // at its own resolution so mixed-meter parts accumulate.
                section_start_row += nb as f64 * ns as f64;

                pos = bracket_close + 1;
                while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
            }
        }

        if pos < bytes.len() && bytes[pos] == b';' {
            pos += 1;
        }

        parts.insert(
            name.clone(),
                     TrackscriptPart {
                         name,
                         sample_path,
                         beats_per_measure,
                         subs_per_beat,
                         len_beats: None,
                         sample_offset,
                         src_bpm,
                         events: all_events,
                         default_vol,
                         default_pan,
                         default_dur,
                     },
        );
    }

    (blocks, parts)
}

// ── top level ──────────────────────────────────────────────────────────

pub fn parse_str(text: &str) -> Result<TrackscriptFile> {
    let text = strip_comments(text);

    let config_rx = Regex::new(r"\bConfig\s*\{").unwrap();
    let config = extract_block(&text, &config_rx)
    .map(parse_config)
    .unwrap_or_default();

    let tempo_rx = Regex::new(r"\bTempo\s*\{").unwrap();
    let tempo = extract_block(&text, &tempo_rx)
    .map(|b| parse_tempo(b, config.subdivisions))
    .unwrap_or_default();

    let bl_rx = Regex::new(r"\bBlockList\s*\([^)]*\)\s*\{").unwrap();
    let (block_list, shims) = extract_block(&text, &bl_rx)
    .map(|b| parse_block_list(b, config.subdivisions))
    .unwrap_or_default();

    let ba_rx = Regex::new(r"\bBlockArray\s*\([^)]*\)\s*\{").unwrap();
    let (blocks, parts) = extract_block(&text, &ba_rx)
    .map(parse_block_array)
    .unwrap_or_default();

    let mut notes = Vec::new();
    for (placement_index, calls) in block_list.iter().enumerate() {
        for call in calls {
            let names: Vec<String> = blocks
            .get(call)
            .cloned()
            .unwrap_or_else(|| vec![call.clone()]);
            for part_name in &names {
                let Some(part) = parts.get(part_name) else { continue };
                let placement_row = placement_index as f64
                * part.beats_per_measure as f64
                * part.subs_per_beat as f64;
                for ev in &part.events {
                    if ev.is_waypoint() {
                        continue;
                    }
                    let (Some(midi), Some(hz)) = (ev.pitch_midi, ev.pitch_hz) else { continue };
                    let dur_rows = ev.dur_beats.map(|d| d * part.subs_per_beat as f64);
                    notes.push(NoteEvent {
                        row: placement_row + ev.row,
                        pitch: midi,
                        pitch_hz: hz,
                        voice: format!("{}@{}", part_name, placement_index),
                               sample: ev
                               .sample_override
                               .clone()
                               .or_else(|| Some(part.sample_path.clone())),
                               duration_rows: dur_rows,
                               duration_beats: ev.dur_beats,
                               section: placement_index,
                               source: format!("TS row {} {}", placement_row + ev.row, part_name),
                    });
                }
            }
        }
    }
    notes.sort_by(|a, b| {
        a.row
        .partial_cmp(&b.row)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(a.pitch.cmp(&b.pitch))
        .then(a.voice.cmp(&b.voice))
    });

    Ok(TrackscriptFile {
        config,
       tempo_events: tempo,
       block_list,
       shims,
       blocks,
       parts,
       notes,
    })
}

pub fn parse_file(path: &Path) -> Result<TrackscriptFile> {
    let text = fs::read_to_string(path)
    .map_err(|e| anyhow!("reading {}: {}", path.display(), e))?;
    parse_str(&text)
}
