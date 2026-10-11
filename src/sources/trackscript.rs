//! Trackscript parser — Rust port of trackscript_parser.py.
//!
//! Exposes the same public surface as the Python module:
//!   parse_file(path) -> TrackscriptFile
//!
//! Pitch is exposed twice: pitch_midi (for comparisons) and pitch_hz
//! (for the audio renderer).  Both come from the same source text.
//!
//! Behaviour is kept deliberately identical to the Python parser, quirks
//! included (e.g. measure numbers come from counting newlines in the
//! *stripped* body up to the match start).

use anyhow::{anyhow, Result};
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

pub const C5_HZ: f64 = 523.2511306;
#[allow(dead_code)]
pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

// ── data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TrackscriptEvent {
    pub op: String, // "", "&", "=", "▲", "♪"
    pub row: f64,
    pub pitch_midi: Option<i32>,
    pub pitch_hz: Option<f64>,
    pub vol: Option<f64>,
    pub pan: Option<f64>,
    pub dur_beats: Option<f64>,
    pub sample_override: Option<String>,
    /// Some(n) whenever a ƒ'path' source is present: `ƒ'p'()` -> Some(0),
    /// `ƒ'p'(123)` -> Some(123).  None only when there is no ƒ field.
    pub offset: Option<u32>,
    /// The literal `<...>` text for this event, without the prefix op.
    pub raw: String,
}

impl TrackscriptEvent {
    pub fn is_waypoint(&self) -> bool {
        self.op == "&"
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TrackscriptPart {
    pub name: String,
    pub sample_path: String,
    pub beats_per_measure: u32,
    pub subs_per_beat: u32,
    pub len_beats: Option<f64>,
    pub sample_offset: u32,
    pub events: Vec<TrackscriptEvent>,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Config {
    pub bpm: f64,
    pub subdivisions: u32,
    pub time_signature: (u32, u32),
    pub song_volume: f64,
    pub panning_law: String, // trimmed + lowercased, like the Python player
    pub raw: HashMap<String, String>,
}

#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct TrackscriptFile {
    pub config: Config,
    pub tempo_events: Vec<(f64, f64, f64)>, // (row, bpm, ramp_beats)
    pub block_list: Vec<Vec<String>>,
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

/// Equivalent of the Python regex `([A-Ga-g])([#b]?)(-?\d+)` (fullmatch).
fn parse_note_name(text: &str) -> Option<i32> {
    let text = text.trim();
    let mut chars = text.chars();
    let first = chars.next()?.to_ascii_uppercase();
    let base = match first {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
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

/// Byte index of the matching close delimiter for `text[start] == open_ch`.
/// Returns `text.len()` if unmatched (same as the Python version).
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

/// Find `header_rx { ... }` and return the body between the braces.
/// Every header regex ends in `\{`, so the brace is the last byte of the match.
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

// ── sub-parsers ────────────────────────────────────────────────────────

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

/// Returns [(absolute_row, bpm, ramp_beats)]. Each line of the block is a
/// measure of 4 beats.
fn parse_tempo(body: &str, subs: u32) -> Vec<(f64, f64, f64)> {
    static EVENT_RX: OnceLock<Regex> = OnceLock::new();
    let event_rx = EVENT_RX.get_or_init(|| Regex::new(r"&?\s*<([^<>]+)>").unwrap());

    let body = body.trim();
    let measure_rows = 4.0 * subs as f64;
    let mut events = Vec::new();

    for em in event_rx.captures_iter(body) {
        let inner = em.get(1).unwrap();
        let mut row: Option<f64> = None;
        let mut bpm: Option<f64> = None;
        let mut ramp = 0.0f64;

        for f in inner.as_str().split(',').map(str::trim) {
            if let Some(rest) = f.strip_prefix('§') {
                row = None;
                if let Some((a, b)) = rest.split_once('/') {
                    if let (Some(ai), Some(bi)) = (parse_f(a), parse_f(b)) {
                        let local = (ai - 1.0) * subs as f64 + bi - 1.0;
                        // Python: body.count('\n', 0, em.start(1)) -- the
                        // capture start, NOT the whole-match start.  The
                        // whole match can begin on the newline that
                        // precedes the event, which would undercount.
                        let measure = body[..inner.start()].matches('\n').count() as f64;
                        row = Some(measure * measure_rows + local);
                    }
                }
            } else if let Some(rest) = f.strip_prefix('#') {
                if let Some(v) = parse_f(rest) {
                    bpm = Some(v);
                }
            } else if let Some(rest) = f.strip_prefix('~') {
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

/// Each line is a list of block names that play together.
fn parse_block_list(body: &str) -> Vec<Vec<String>> {
    static NAME_RX: OnceLock<Regex> = OnceLock::new();
    let name_rx = NAME_RX.get_or_init(|| Regex::new(r"ƒ([A-Za-z_][A-Za-z0-9_]*)").unwrap());

    let mut lines = Vec::new();
    for raw in body.trim().split('\n') {
        let names: Vec<String> = name_rx
            .captures_iter(raw)
            .map(|c| c[1].to_string())
            .collect();
        if !names.is_empty() || raw.trim() == ";" {
            lines.push(names);
        }
    }
    lines
}

fn parse_part_events(body: &str, subs: u32, beats_per_measure: u32) -> Vec<TrackscriptEvent> {
    static EVENT_RX: OnceLock<Regex> = OnceLock::new();
    static SRC_RX: OnceLock<Regex> = OnceLock::new();
    let event_rx = EVENT_RX
        .get_or_init(|| Regex::new(r"(?P<op>[♪&=▲]?)\s*<(?P<body>[^<>]*)>").unwrap());
    let src_rx = SRC_RX
        .get_or_init(|| Regex::new(r"^ƒ'([^']+)'(?:\((\d*)\)|\+(\d+))?").unwrap());

    let body = body.trim();
    let mut events = Vec::new();

    for em in event_rx.captures_iter(body) {
        let op = em.name("op").map(|m| m.as_str()).unwrap_or("").to_string();
        let inner = em.name("body").map(|m| m.as_str()).unwrap_or("");
        let m0 = em.get(0).unwrap();

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
            raw: format!("<{}>", inner),
        };

        let mut beat: Option<f64> = None;
        let mut sub: Option<f64> = None;

        for f in split_fields(inner) {
            if let Some(rest) = f.strip_prefix('§') {
                beat = None;
                sub = None;
                if let Some((a, b)) = rest.split_once('/') {
                    if let (Some(ai), Some(bi)) = (parse_f(a), parse_f(b)) {
                        beat = Some(ai);
                        sub = Some(bi - 1.0);
                    }
                }
            } else if let Some(rest) = f.strip_prefix('#') {
                ev.pitch_midi = parse_pitch_midi(rest);
                ev.pitch_hz = parse_pitch_hz(rest);
            } else if let Some(rest) = f.strip_prefix('~') {
                if let Some(v) = parse_f(rest.trim_start_matches('+')) {
                    ev.dur_beats = Some(v);
                }
            } else if let Some(rest) = f.strip_prefix('%') {
                if let Some(v) = parse_f(rest.trim_start_matches('+')) {
                    ev.vol = Some(v);
                }
            } else if let Some(rest) = f.strip_prefix('¶') {
                if let Some(v) = parse_f(rest) {
                    ev.pan = Some(v);
                }
            } else if f.starts_with('ƒ') {
                // ƒ'path'() or ƒ'path'(offset); old ƒ'path'+offset still reads.
                // As in Python, an explicit source always sets an offset (0 if
                // none was written), which overrides the part's default offset.
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
        let measure = body[..m0.start()].matches('\n').count() as f64;
        ev.row = measure * beats_per_measure as f64 * subs as f64 + (b - 1.0) * subs as f64 + s;
        events.push(ev);
    }

    events
}

type BlockMap = HashMap<String, Vec<String>>;
type PartMap = HashMap<String, TrackscriptPart>;

fn parse_block_array(body: &str) -> (BlockMap, PartMap) {
    static NAME_RX: OnceLock<Regex> = OnceLock::new();
    static CALL_RX: OnceLock<Regex> = OnceLock::new();
    static SRC_RX: OnceLock<Regex> = OnceLock::new();
    static HEADER_RX: OnceLock<Regex> = OnceLock::new();
    let name_rx = NAME_RX.get_or_init(|| Regex::new(r"([A-Za-z_][A-Za-z0-9_]*)\s*=\s*").unwrap());
    let call_rx = CALL_RX.get_or_init(|| Regex::new(r"ƒ([A-Za-z_][A-Za-z0-9_]*)").unwrap());
    // Both of these are anchored (Python uses re.match on them).
    let src_rx =
        SRC_RX.get_or_init(|| Regex::new(r"^ƒ'([^']+)'(?:\((\d*)\))?\s*:\s*").unwrap());
    let header_rx = HEADER_RX
        .get_or_init(|| Regex::new(r"^\s*@(\d+)\s*/\s*(\d+)\s*(?:~([\d.]+)\s*)?\[").unwrap());

    let mut blocks = BlockMap::new();
    let mut parts = PartMap::new();
    let bytes = body.as_bytes();
    let mut pos = 0usize;

    while pos < body.len() {
        let Some(caps) = name_rx.captures(&body[pos..]) else { break };
        let name = caps[1].to_string();
        let mut j = pos + caps.get(0).unwrap().end();
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\n') {
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }

        if bytes[j] == b'{' {
            // Composite block: name = { ƒP1, ƒP2; }
            let close = match_pair(body, j, b'{', b'}');
            let inner = &body[j + 1..close];
            let names: Vec<String> = call_rx
                .captures_iter(inner)
                .map(|c| c[1].to_string())
                .collect();
            blocks.insert(name, names);
            pos = close + 1;
        } else if body[j..].starts_with('ƒ') {
            // Part: name = ƒ'path'(): { @N/D[ ... ] }
            let Some(sm) = src_rx.captures(&body[j..]) else {
                pos = next_char_boundary(body, j);
                continue;
            };
            let sample_path = sm[1].to_string();
            let sample_offset: u32 = sm
                .get(2)
                .map(|m| m.as_str())
                .unwrap_or("")
                .parse()
                .unwrap_or(0);
            let mut k = j + sm.get(0).unwrap().end();
            while k < bytes.len() && matches!(bytes[k], b' ' | b'\t' | b'\n') {
                k += 1;
            }
            if k >= bytes.len() || bytes[k] != b'{' {
                pos = next_char_boundary(body, j);
                continue;
            }
            let close = match_pair(body, k, b'{', b'}');
            let inner = &body[k + 1..close];

            let Some(hm) = header_rx.captures(inner) else {
                pos = close + 1;
                continue;
            };
            let (Ok(beats), Ok(subs)) = (hm[1].parse::<u32>(), hm[2].parse::<u32>()) else {
                pos = close + 1;
                continue;
            };
            let len_beats: Option<f64> = hm.get(3).and_then(|m| m.as_str().parse().ok());
            let open_bracket = hm.get(0).unwrap().end() - 1;
            let close_bracket = match_pair(inner, open_bracket, b'[', b']');
            let event_body = &inner[open_bracket + 1..close_bracket];

            let events = parse_part_events(event_body, subs, beats);
            parts.insert(
                name.clone(),
                TrackscriptPart {
                    name,
                    sample_path,
                    beats_per_measure: beats,
                    subs_per_beat: subs,
                    len_beats,
                    sample_offset,
                    events,
                },
            );
            pos = close + 1;
        } else {
            pos = next_char_boundary(body, j);
        }
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
    let block_list = extract_block(&text, &bl_rx)
        .map(parse_block_list)
        .unwrap_or_default();

    let ba_rx = Regex::new(r"\bBlockArray\s*\([^)]*\)\s*\{").unwrap();
    let (blocks, parts) = extract_block(&text, &ba_rx)
        .map(parse_block_array)
        .unwrap_or_default();

    // Flatten note-ons (diagnostics; the player walks parts directly).
    let mut notes = Vec::new();
    for (placement_index, calls) in block_list.iter().enumerate() {
        for call in calls {
            let names: Vec<String> = blocks
                .get(call)
                .cloned()
                .unwrap_or_else(|| vec![call.clone()]);
            for part_name in &names {
                let Some(part) = parts.get(part_name) else { continue };
                let placement_row =
                    placement_index as f64 * part.beats_per_measure as f64 * part.subs_per_beat as f64;
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
        blocks,
        parts,
        notes,
    })
}

pub fn parse_file(path: &Path) -> Result<TrackscriptFile> {
    let text = fs::read_to_string(path).map_err(|e| anyhow!("reading {}: {}", path.display(), e))?;
    parse_str(&text)
}
