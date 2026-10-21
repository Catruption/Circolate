//! Trackscript parser.
//!
//! 1.11 syntax:
//!
//!   ƒNAME(<|N/D, args>)[ ... ];         part definition (flat body)
//!   ƒNAME(<|N/D, args>)                 part with named sub-blocks
//!       ƒSubA()[ ... ]
//!       ƒSubB()[ ... ]
//!   ;
//!   ƒNAME(ƒA, ƒB);                      composite block
//!   ƒNAME();  ƒNAME(<args>);            call in BlockList
//!   ƒNAME(1,2,3);                       call with measure selection
//!   @<§pos, ~len, ◊mix, %gain, ¶pan, ƒTarget(params)>;   shim event
//!   ƒName(args)@<ƒTarget(params)>;      call-attached shim
//!
//! Args use the same field list as notes.  Fields a definition accepts
//! become its defaults for that field; a note that omits a field
//! inherits the default.
//!
//! A part body is one of:
//!
//!   * `[ measures... ]`      — a flat sequence of measures
//!   * `ƒSub()[ ... ] ...`    — one or more named sub-blocks
//!
//! Each measure is one line.  A line whose last non-whitespace char is
//! `,` continues into the next line, so long measures can wrap.  A new
//! sub-block becomes an addressable part of its own, inheriting the
//! parent's args (sample path, frame, defaults).  Calling `ƒName(N)`
//! from BlockList selects measure N of the flattened measure list,
//! counting sub-blocks in the order they appear.
//!
//! Old `|N/D[...]` multi-section bodies still parse, for files written
//! before this syntax.

use anyhow::{anyhow, Result};
use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use crate::spec::{
    active_event_prefix_glyphs, parse_greek_slot, FIELD_DURATION, FIELD_FINE_PITCH,
    FIELD_MIX, FIELD_MOD_X, FIELD_MOD_Y, FIELD_PAN, FIELD_PITCH, FIELD_POSITION,
    FIELD_RELEASE, FIELD_SOURCE, FIELD_VOLUME, PART_HEADER_SIGIL,
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
    pub src_bpm: Option<f64>,
    pub fine_pitch: Option<i16>,
    pub release: Option<i16>,
    pub mod_x: Option<i16>,
    pub mod_y: Option<i16>,
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
    pub src_bpm: Option<f64>,
    pub events: Vec<TrackscriptEvent>,
    pub measures: Vec<Vec<TrackscriptEvent>>,
    pub section_lengths: Vec<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_vol: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_pan: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_dur: Option<f64>,
}

// ── shim events ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ShimParam {
    pub slot: u32,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum ShimField {
    Mix,
    Gain,
    Pan,
    Pitch,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShimWaypoint {
    pub row: f64,
    pub field: ShimField,
    pub value: f64,
    pub ramp_rows: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShimEvent {
    pub line: usize,
    pub row: f64,
    pub target: String,
    pub params: Vec<ShimParam>,
    pub length_rows: Option<f64>,
    pub mix: f64,
    pub gain: f64,
    pub pan: f64,
    pub attached_call: Option<usize>,
    pub waypoints: Vec<ShimWaypoint>,
    // Property-shim fields for `ƒTempo` and future `ƒGlobal*`:
    pub property: String,
    pub start_value: f64,
    pub op: String,
    pub amount: f64,
    pub interval_rows: f64,
    pub count: Option<u32>,
}

impl ShimEvent {
    /// Effective start row of the shim's window.  For call-attached shims
    /// the row is 0 until the BlockList cursor is resolved by the loader.
    pub fn effective_row(&self) -> f64 {
        self.row
    }
}

// ── other events ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct BlockCall {
    pub name: String,
    pub selections: Vec<MeasureSelection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MeasureSelection {
    pub measure: usize,
    pub overrides: Option<CallOverrides>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct CallOverrides {
    pub vol: Option<f64>,
    pub pan: Option<f64>,
    pub dur: Option<f64>,
    pub source: Option<String>,
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
    pub block_list: Vec<Vec<BlockCall>>,
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

/// Split on commas at the top level of `body`.  Parentheses and quotes
/// protect commas — this lets `ƒReverb(α1, γ8)` survive as one field.
fn split_fields(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut paren_depth: i32 = 0;
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
            '(' => {
                paren_depth += 1;
                cur.push(c);
            }
            ')' => {
                paren_depth = (paren_depth - 1).max(0);
                cur.push(c);
            }
            ',' if paren_depth == 0 => {
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
        Regex::new(&format!(r"{src}([A-Za-z_][A-Za-z0-9_.-]*)")).unwrap()
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

// ── shim parsing ───────────────────────────────────────────────────────

/// Given `§a/b`, return `(a-1)*subs + (b-1)`.  Non-numeric returns None.
fn parse_position(s: &str, subs: u32) -> Option<f64> {
    let (a, b) = s.split_once('/')?;
    let ai = parse_f(a)?;
    let bi = parse_f(b)?;
    Some((ai - 1.0) * subs as f64 + (bi - 1.0))
}

/// Find the index of the first un-parenthesized, un-quoted `>` starting
/// at `start`.  Used to find the end of a shim body, where the target
/// may itself contain `(params)`.
fn find_shim_close(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut pos = start;
    let mut in_quote = false;
    let mut paren = 0i32;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\'' => in_quote = !in_quote,
            b'(' if !in_quote => paren += 1,
            b')' if !in_quote => paren = (paren - 1).max(0),
            b'>' if !in_quote && paren == 0 => return Some(pos),
            _ => {}
        }
        pos += 1;
    }
    None
}

/// Split `Fruity_Reeverb_2(α25, β5)` into name and args.
fn split_target(s: &str) -> (String, &str) {
    let s = s.trim();
    if let Some(open) = s.find('(') {
        let name = s[..open].trim().to_string();
        let close = s[open..].rfind(')').map(|i| open + i).unwrap_or(s.len());
        (name, &s[open + 1..close])
    } else {
        (s.to_string(), "")
    }
}

fn parse_effect_params(args: &str) -> Vec<ShimParam> {
    let mut out = Vec::new();
    for part in args.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (slot, consumed) = parse_greek_slot(part);
        if slot == 0 {
            continue;
        }
        if let Some(v) = parse_f(&part[consumed..]) {
            out.push(ShimParam { slot, value: v });
        }
    }
    out
}

/// Parse one `@<...>` body into a `ShimEvent`.  `attached_call` is the
/// index of the preceding call on the same BlockList line, or None if
/// the shim stands alone.
fn parse_shim_event(
    inner: &str,
    line: usize,
    subs: u32,
    attached_call: Option<usize>,
) -> Option<ShimEvent> {
    let fields = split_fields(inner);
    let mut row: Option<f64> = None;
    let mut length_rows: Option<f64> = None;
    let mut mix: Option<f64> = None;
    let mut target = String::new();
    let mut params: Vec<ShimParam> = Vec::new();
    let mut percent_value: Option<f64> = None;
    let mut pan_value: Option<f64> = None;
    let mut pitch_value: Option<f64> = None;

    for f in fields {
        if let Some(rest) = f.strip_prefix(FIELD_SOURCE) {
            let (name, args) = split_target(&rest);
            target = name;
            params = parse_effect_params(&args);
        } else if let Some(rest) = f.strip_prefix(FIELD_POSITION) {
            row = parse_position(&rest, subs);
        } else if let Some(rest) = f.strip_prefix(FIELD_DURATION) {
            length_rows = parse_f(rest.trim_start_matches('+'))
            .map(|b| b * subs as f64);
        } else if let Some(rest) = f.strip_prefix(FIELD_MIX) {
            mix = parse_f(rest.trim_start_matches('+')).map(|v| v.clamp(0.0, 1.0));
        } else if let Some(rest) = f.strip_prefix(FIELD_VOLUME) {
            percent_value = parse_f(rest.trim_start_matches('+'));
        } else if let Some(rest) = f.strip_prefix(FIELD_PAN) {
            pan_value = parse_f(rest);
        } else if let Some(rest) = f.strip_prefix(FIELD_PITCH) {
            pitch_value = parse_f(rest.trim_start_matches('+'));
        }
    }

    if target.is_empty() {
        return None;
    }

    let is_global = crate::spec::is_reserved(&target);

    // For a reserved global, `%`/`¶`/`#` are the shim's *property*, not
    // universal gain/pan.  For an effect target, they stay universal.
    let (gain, pan, property, start_value) = if is_global {
        let (prop, val) = if let Some(v) = percent_value {
            (FIELD_VOLUME.to_string(), v)
        } else if let Some(v) = pitch_value {
            (FIELD_PITCH.to_string(), v)
        } else if let Some(v) = pan_value {
            (FIELD_PAN.to_string(), v)
        } else {
            (String::new(), 0.0)
        };
        (100.0, 0.0, prop, val)
    } else {
        let gain = percent_value.unwrap_or(100.0);
        let pan = pan_value.unwrap_or(0.0);
        (gain, pan, String::new(), 0.0)
    };

    // Call-attached: window defaults to the call's duration; ◊ defaults
    // to 1.0 (wet).  Standalone: ◊ defaults to 0.0 (dry until ramped).
    let default_mix = if attached_call.is_some() { 1.0 } else { 0.0 };

    Some(ShimEvent {
        line,
         row: row.unwrap_or(0.0),
         target,
         params,
         length_rows,
         mix: mix.unwrap_or(default_mix),
         gain,
         pan,
         attached_call,
         waypoints: Vec::new(),
         property,
         start_value,
         op: String::new(),
         amount: 0.0,
         interval_rows: 0.0,
         count: None,
    })
}

/// Parse a `&<...>` waypoint that follows a shim and attach it.
fn parse_shim_waypoint_into(inner: &str, shim: &mut ShimEvent, subs: u32) {
    let fields = split_fields(inner);
    let mut row: Option<f64> = None;
    let mut field: Option<ShimField> = None;
    let mut value: Option<f64> = None;
    let mut ramp_rows: Option<f64> = None;

    for f in fields {
        if let Some(rest) = f.strip_prefix(FIELD_POSITION) {
            row = parse_position(&rest, subs);
        } else if let Some(rest) = f.strip_prefix(FIELD_MIX) {
            field = Some(ShimField::Mix);
            value = parse_f(rest.trim_start_matches('+'));
        } else if let Some(rest) = f.strip_prefix(FIELD_VOLUME) {
            field = Some(ShimField::Gain);
            value = parse_f(rest.trim_start_matches('+'));
        } else if let Some(rest) = f.strip_prefix(FIELD_PAN) {
            field = Some(ShimField::Pan);
            value = parse_f(rest);
        } else if let Some(rest) = f.strip_prefix(FIELD_DURATION) {
            ramp_rows = parse_f(rest.trim_start_matches('+')).map(|v| v * subs as f64);
        }
    }

    if let (Some(r), Some(fld), Some(v)) = (row, field, value) {
        shim.waypoints.push(ShimWaypoint {
            row: r,
            field: fld,
            value: v,
            ramp_rows,
        });
    }
}

// ── block list ─────────────────────────────────────────────────────────

fn parse_block_list(body: &str, subs: u32) -> (Vec<Vec<BlockCall>>, Vec<ShimEvent>) {
    let mut lines = Vec::new();
    let mut shims = Vec::new();

    for raw in body.trim().split('\n') {
        if raw.trim().is_empty() {
            continue;
        }
        let line_idx = lines.len();
        let (calls, line_shims) = parse_block_line(raw, line_idx, subs);
        shims.extend(line_shims);
        lines.push(calls);
    }

    (lines, shims)
}

/// Parse one BlockList line into calls and any shims on it.  Each shim
/// is attached to the most recently parsed call (or None for standalone
/// shims at the start of a line).
fn parse_block_line(text: &str, line_idx: usize, subs: u32) -> (Vec<BlockCall>, Vec<ShimEvent>) {
    let mut calls: Vec<BlockCall> = Vec::new();
    let mut shims: Vec<ShimEvent> = Vec::new();
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let mut last_call: Option<usize> = None;

    while pos < bytes.len() {
        let b = bytes[pos];

        if b.is_ascii_whitespace() || b == b';' {
            pos += 1;
            continue;
        }

        // ƒName(args)  — a call
        if text[pos..].starts_with(FIELD_SOURCE) {
            let after_src = pos + FIELD_SOURCE.len();
            let name_end = text[after_src..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != '.')
            .map(|i| after_src + i)
            .unwrap_or(text.len());
            let name = text[after_src..name_end].to_string();
            if name.is_empty() {
                pos = after_src;
                continue;
            }
            pos = name_end;
            let mut selections = Vec::new();
            if pos < bytes.len() && bytes[pos] == b'(' {
                let close = match_pair(text, pos, b'(', b')');
                selections = parse_call_selections(&text[pos + 1..close]);
                pos = close + 1;
            }
            calls.push(BlockCall { name, selections });
            last_call = Some(calls.len() - 1);
            continue;
        }

        // @<...>  — a shim
        if text[pos..].starts_with("@<") {
            let inner_start = pos + 2;
            let Some(close) = find_shim_close(text, inner_start) else { break };
            let inner = &text[inner_start..close];
            pos = close + 1;
            if let Some(mut shim) = parse_shim_event(inner, line_idx, subs, last_call) {
                // Trailing waypoints &<...>
                while pos < bytes.len() && text[pos..].starts_with("&<") {
                    let wp_start = pos + 2;
                    let Some(wp_close) = find_shim_close(text, wp_start) else { break };
                    parse_shim_waypoint_into(&text[wp_start..wp_close], &mut shim, subs);
                    pos = wp_close + 1;
                }
                shims.push(shim);
            }
            continue;
        }

        pos += 1;
    }

    (calls, shims)
}

/// Parse every `ƒName(args)` call out of a BlockList line (legacy
/// helper kept for callers that only need the calls).
#[allow(dead_code)]
fn parse_block_calls(text: &str) -> Vec<BlockCall> {
    parse_block_line(text, 0, 4).0
}

fn parse_call_selections(args: &str) -> Vec<MeasureSelection> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for item in split_fields(trimmed) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (num_str, ov_str) = if let Some(lt) = item.find('<') {
            (&item[..lt], Some(&item[lt..]))
        } else {
            (item, None)
        };

        let Ok(n) = num_str.trim().parse::<usize>() else {
            continue;
        };
        if n == 0 {
            continue;
        }
        let overrides = ov_str.and_then(parse_call_overrides);
        out.push(MeasureSelection {
            measure: n - 1,
            overrides,
        });
    }
    out
}

fn parse_call_overrides(text: &str) -> Option<CallOverrides> {
    let inner = text
    .trim_start_matches('<')
    .trim_end_matches('>')
    .trim();
    if inner.is_empty() {
        return None;
    }
    let mut ov = CallOverrides::default();
    let src_rx = source_rx();

    for f in split_fields(inner) {
        if let Some(rest) = f.strip_prefix(FIELD_VOLUME) {
            if let Some(v) = parse_f(rest.trim_start_matches('+')) {
                ov.vol = Some(v);
            }
        } else if let Some(rest) = f.strip_prefix(FIELD_PAN) {
            if let Some(v) = parse_f(rest) {
                ov.pan = Some(v);
            }
        } else if let Some(rest) = f.strip_prefix(FIELD_DURATION) {
            if let Some(v) = parse_f(rest.trim_start_matches('+')) {
                ov.dur = Some(v);
            }
        } else if f.starts_with(FIELD_SOURCE) {
            if let Some(c) = src_rx.captures(&f) {
                ov.source = Some(c[1].to_string());
            }
        }
    }

    if ov == CallOverrides::default() {
        None
    } else {
        Some(ov)
    }
}

// ── part events ────────────────────────────────────────────────────────

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
            fine_pitch: None,
            release: None,
            mod_x: None,
            mod_y: None,
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
            } else if f.starts_with(FIELD_SOURCE) {
                if let Some(c) = src_rx.captures(&f) {
                    ev.sample_override = Some(c[1].to_string());
                    let raw = c
                    .get(2)
                    .filter(|m| !m.as_str().is_empty())
                    .or_else(|| c.get(3))
                    .map(|m| m.as_str())
                    .unwrap_or("");
                    ev.offset = if raw.is_empty() {
                        None
                    } else {
                        raw.parse().ok()
                    };
                }
            }
        }

        let (Some(b), Some(s)) = (beat, sub) else { continue };
        ev.row = absolute_offset_rows + (b - 1.0) * subs as f64 + s;
        events.push(ev);
    }

    events
}

// ── part body parser ───────────────────────────────────────────────────

enum BodyItem {
    Measure(Vec<TrackscriptEvent>),
    SubBlock { name: String, items: Vec<BodyItem> },
}

struct Cursor<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(s: &'a str, pos: usize) -> Self {
        Cursor { s, pos }
    }

    fn eof(&self) -> bool {
        self.pos >= self.s.len()
    }

    fn peek_byte(&self) -> Option<u8> {
        self.s.as_bytes().get(self.pos).copied()
    }

    fn peek(&self) -> Option<char> {
        if self.eof() {
            return None;
        }
        self.s[self.pos..].chars().next()
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.pos += c.len_utf8();
        }
    }

    fn skip_h_ws(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\t') | Some('\r')) {
            self.pos += 1;
        }
    }

    fn skip_all_ws(&mut self) {
        while self.peek().map_or(false, |c| c.is_whitespace()) {
            self.bump();
        }
    }

    fn expect_byte(&mut self, b: u8) -> bool {
        if self.peek_byte() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
}

fn find_matching_gt(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn parse_identifier(c: &mut Cursor) -> String {
    let start = c.pos;
    match c.peek_byte() {
        Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
            c.pos += 1;
        }
        _ => return String::new(),
    }
    while let Some(b) = c.peek_byte() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' {
            c.pos += 1;
        } else {
            break;
        }
    }
    c.s[start..c.pos].to_string()
}

fn parse_measure(c: &mut Cursor, subs: u32, beats: u32) -> Vec<TrackscriptEvent> {
    let mut events = Vec::new();
    loop {
        c.skip_h_ws();
        let start = c.pos;

        if c.peek_byte() == Some(b'&') {
            c.pos += 1;
            c.skip_h_ws();
        }

        if c.peek_byte() != Some(b'<') {
            c.pos = start;
            break;
        }

        let Some(end) = find_matching_gt(c.s, c.pos) else {
            c.pos = start;
            break;
        };
        let ev_str = &c.s[start..end];
        if let Some(ev) = parse_part_events(ev_str, subs, beats, 0.0).into_iter().next() {
            events.push(ev);
        }
        c.pos = end;

        c.skip_h_ws();
        match c.peek_byte() {
            Some(b',') => {
                c.pos += 1;
                c.skip_all_ws();
                if matches!(c.peek_byte(), None | Some(b']')) {
                    break;
                }
            }
            Some(b'\n') => {
                c.pos += 1;
                break;
            }
            Some(b'&') => {
                continue;
            }
            Some(b']') | None => break,
            _ => break,
        }
    }
    events
}

fn parse_body(c: &mut Cursor, subs: u32, beats: u32) -> Vec<BodyItem> {
    let mut items = Vec::new();
    loop {
        c.skip_all_ws();
        if c.eof() {
            break;
        }
        let b = c.peek_byte().unwrap();
        if b == b']' {
            break;
        }
        if b == b'[' {
            c.pos += 1;
            let inner = parse_body(c, subs, beats);
            if !c.expect_byte(b']') {
                break;
            }
            items.extend(inner);
            continue;
        }
        if c.s[c.pos..].starts_with(FIELD_SOURCE) {
            c.pos += FIELD_SOURCE.len();
            let name = parse_identifier(c);
            if name.is_empty() {
                break;
            }
            c.skip_all_ws();
            if !c.expect_byte(b'(') {
                break;
            }
            c.skip_all_ws();
            if !c.expect_byte(b')') {
                break;
            }
            c.skip_all_ws();
            if !c.expect_byte(b'[') {
                break;
            }
            let sub_items = parse_body(c, subs, beats);
            if !c.expect_byte(b']') {
                break;
            }
            items.push(BodyItem::SubBlock { name, items: sub_items });
            continue;
        }
        if b == b'<' || b == b'&' {
            let evs = parse_measure(c, subs, beats);
            if evs.is_empty() {
                break;
            }
            items.push(BodyItem::Measure(evs));
            continue;
        }
        break;
    }
    items
}

fn flatten_body_items(items: &[BodyItem], out: &mut Vec<Vec<TrackscriptEvent>>) {
    for item in items {
        match item {
            BodyItem::Measure(events) => out.push(events.clone()),
            BodyItem::SubBlock { items, .. } => flatten_body_items(items, out),
        }
    }
}

fn register_sub_blocks(
    items: &[BodyItem],
    args: &PartArgs,
    parts: &mut HashMap<String, TrackscriptPart>,
) {
    for item in items {
        if let BodyItem::SubBlock { name, items: sub_items } = item {
            let mut measures = Vec::new();
            flatten_body_items(sub_items, &mut measures);
            let frame_len = args.beats_per_measure as f64 * args.subs_per_beat as f64;
            let section_lengths = vec![frame_len; measures.len()];
            parts.insert(
                name.clone(),
                         make_part(name.clone(), args, measures, section_lengths),
            );
            register_sub_blocks(sub_items, args, parts);
        }
    }
}

struct PartArgs {
    sample_path: String,
    sample_offset: u32,
    src_bpm: Option<f64>,
    beats_per_measure: u32,
    subs_per_beat: u32,
    default_vol: Option<f64>,
        default_pan: Option<f64>,
            default_dur: Option<f64>,
}

fn make_part(
    name: String,
    args: &PartArgs,
    measures: Vec<Vec<TrackscriptEvent>>,
    section_lengths: Vec<f64>,
) -> TrackscriptPart {
    let mut events = Vec::new();
    let mut offset = 0.0;
    for (i, evs) in measures.iter().enumerate() {
        for ev in evs {
            let mut e = ev.clone();
            e.row += offset;
            events.push(e);
        }
        offset += section_lengths.get(i).copied().unwrap_or(0.0);
    }
    TrackscriptPart {
        name,
        sample_path: args.sample_path.clone(),
        beats_per_measure: args.beats_per_measure,
        subs_per_beat: args.subs_per_beat,
        len_beats: None,
        sample_offset: args.sample_offset,
        src_bpm: args.src_bpm,
        events,
        measures,
        section_lengths,
        default_vol: args.default_vol,
            default_pan: args.default_pan,
                default_dur: args.default_dur,
    }
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

        if !body[pos..].starts_with(FIELD_SOURCE) {
            pos = next_char_boundary(body, pos);
            continue;
        }
        let name_start = pos + FIELD_SOURCE.len();
        let name_end = body[name_start..]
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != '.')
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

        // composite
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

        let args = PartArgs {
            sample_path,
            sample_offset,
            src_bpm,
            beats_per_measure,
            subs_per_beat,
            default_vol,
            default_pan,
            default_dur,
        };

        let mut measures: Vec<Vec<TrackscriptEvent>> = Vec::new();
        let mut section_lengths: Vec<f64> = Vec::new();
        let mut body_items: Vec<BodyItem> = Vec::new();

        if pos < bytes.len() && bytes[pos] == PART_HEADER_SIGIL.as_bytes()[0] {
            // Old multi-section body
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

                let (sec_b, sec_s) = if frame_from_args {
                    (beats_per_measure, subs_per_beat)
                } else {
                    (nb, ns)
                };

                let bracket_close = match_pair(body, bracket_open, b'[', b']');
                let events_str = &body[bracket_open + 1..bracket_close];
                let evs = parse_part_events(events_str, sec_s, sec_b, 0.0);
                measures.push(evs);
                section_lengths.push(sec_b as f64 * sec_s as f64);

                pos = bracket_close + 1;
                while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
            }
        } else {
            let mut cur = Cursor::new(body, pos);
            body_items = parse_body(&mut cur, subs_per_beat, beats_per_measure);
            pos = cur.pos;

            flatten_body_items(&body_items, &mut measures);
            let frame_len = beats_per_measure as f64 * subs_per_beat as f64;
            section_lengths = vec![frame_len; measures.len()];
        }

        if pos < bytes.len() && bytes[pos] == b';' {
            pos += 1;
        }

        parts.insert(
            name.clone(),
                     make_part(name, &args, measures, section_lengths),
        );
        register_sub_blocks(&body_items, &args, &mut parts);
    }

    (blocks, parts)
}

// ── top level ──────────────────────────────────────────────────────────

fn section_rx(keyword: &str) -> Regex {
    Regex::new(&format!(
        r"\b{}\s*(?:\([^)]*\))?\s*\{{",
                        regex::escape(keyword)
    ))
    .unwrap()
}

pub fn parse_str(text: &str) -> Result<TrackscriptFile> {
    let text = strip_comments(text);

    let config_rx = section_rx("Config");
    let config = extract_block(&text, &config_rx)
    .map(parse_config)
    .unwrap_or_default();

    let tempo_rx = section_rx("Tempo");
    let tempo = extract_block(&text, &tempo_rx)
    .map(|b| parse_tempo(b, config.subdivisions))
    .unwrap_or_default();

    let bl_rx = section_rx("BlockList");
    let (block_list, shims) = extract_block(&text, &bl_rx)
    .map(|b| parse_block_list(b, config.subdivisions))
    .unwrap_or_default();

    let ba_rx = section_rx("BlockArray");
    let (blocks, parts) = extract_block(&text, &ba_rx)
    .map(parse_block_array)
    .unwrap_or_default();

    let mut notes = Vec::new();
    for (placement_index, calls) in block_list.iter().enumerate() {
        for call in calls {
            let names: Vec<String> = blocks
            .get(&call.name)
            .cloned()
            .unwrap_or_else(|| vec![call.name.clone()]);
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

    if std::env::var_os("TS_DEBUG").is_some() {
        eprintln!("ts: {} parts parsed, {} shims", parts.len(), shims.len());
    }

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
