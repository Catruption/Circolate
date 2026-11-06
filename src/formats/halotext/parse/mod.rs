//! Halotext parser.
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
//!   @<ƒA(params), ƒB(params)>;          chained shim (comma-separated)
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
//! before this syntax.  The retired `Tempo { ... }` section no longer
//! parses; use the `ƒTempo` shim instead.  BlockList accepts either
//! `{ ... }` (1.10) or `[ ... ]` (1.11) as its delimiters.

mod blocks;
mod body;
mod events;
mod pitch;
mod shim;

pub use pitch::*;
pub use crate::formats::halotext::model::*;

pub(crate) use blocks::{parse_block_array, parse_block_list};
pub(crate) use events::parse_part_events;

use anyhow::{anyhow, Result};
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use crate::formats::halotext::spec::{
    active_event_prefix_glyphs, FIELD_DURATION, FIELD_SOURCE, PART_HEADER_SIGIL,
};

// ── low-level text utilities ───────────────────────────────────────────

pub(crate) fn parse_f(s: &str) -> Option<f64> {
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

pub(crate) fn match_pair(text: &str, start: usize, open_ch: u8, close_ch: u8) -> usize {
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
    let open = *text.as_bytes().get(brace)?;
    let (open_ch, close_ch) = match open {
        b'{' => (b'{', b'}'),
        b'[' => (b'[', b']'),
        _ => return None,
    };
    let close = match_pair(text, brace, open_ch, close_ch);
    Some(&text[brace + 1..close])
}

/// Split on commas at the top level of `body`.  Parentheses and quotes
/// protect commas — this lets `ƒReverb(α1, γ8)` survive as one field.
pub(crate) fn split_fields(body: &str) -> Vec<String> {
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

pub(crate) fn next_char_boundary(s: &str, i: usize) -> usize {
    i + s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)
}

// ── spec-driven regexes ────────────────────────────────────────────────

pub(crate) fn event_rx() -> &'static Regex {
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

/// `ƒ'path'`, `ƒ'path'(args)`, or `ƒ'path'+N`.
/// Args may contain any characters except `)` (positional integers,
/// `bank, preset` pairs, or `key: value` named args).
pub(crate) fn source_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        let src = regex::escape(FIELD_SOURCE);
        Regex::new(&format!(r"^{src}'([^']+)'(?:\(([^)]*)\)|\+(\d+))?")).unwrap()
    })
}

pub(crate) fn call_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        let src = regex::escape(FIELD_SOURCE);
        Regex::new(&format!(r"{src}([A-Za-z_][A-Za-z0-9_.-]*)")).unwrap()
    })
}

#[allow(dead_code)]
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

// ── config ─────────────────────────────────────────────────────────────

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
            "StereoWidth" => { // <--- NEW
                if let Some(v) = parse_f(value) {
                    cfg.stereo_width = v as f32;
                }
            }
            _ => {}
        }
    }
    cfg
}

// ── fallbacks ──────────────────────────────────────────────────────────

fn parse_fallbacks(body: &str) -> HashMap<String, FallbackSpec> {
    let mut out = HashMap::new();
    let entry_rx = Regex::new(r"'([^']+)'\s*:\s*\{([^}]*)\}").unwrap();
    let use_rx = Regex::new(r"Use\s*:\s*'([^']+)'").unwrap();
    let gain_rx = Regex::new(r"Gain\s*:\s*([\d.]+)").unwrap();
    for cap in entry_rx.captures_iter(body) {
        let name = cap[1].to_string();
        let fields = &cap[2];
        let Some(use_m) = use_rx.captures(fields) else { continue };
        let use_ = use_m[1].to_string();
        let gain = gain_rx
            .captures(fields)
            .and_then(|c| c[1].parse::<f64>().ok());
        out.insert(name, FallbackSpec { use_, gain });
    }
    out
}

// ── top level ──────────────────────────────────────────────────────────

fn section_rx(keyword: &str) -> Regex {
    Regex::new(&format!(
        r"\b{}\s*(?:\([^)]*\))?\s*[\{{\[]",
        regex::escape(keyword)
    ))
    .unwrap()
}

pub fn parse_str(text: &str) -> Result<HalotextFile> {
    let text = strip_comments(text);
    let mut source_line_starts = vec![0usize];
    source_line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));

    let config_rx = section_rx("Config");
    let config = extract_block(&text, &config_rx)
        .map(parse_config)
        .unwrap_or_default();

    // `Tempo { ... }` is retired in 1.11; `ƒTempo` shims drive tempo.
    // Old sections are ignored (the parser doesn't look for them).  The
    // migration script converts `Tempo { BeatsPerMinute: N; }` into
    // `@<§1/1,#N, ƒTempo>;` in the BlockList.
    let tempo: Vec<(f64, f64, f64)> = Vec::new();

    let fb_rx = section_rx("Fallbacks");
    let fallbacks = extract_block(&text, &fb_rx)
        .map(parse_fallbacks)
        .unwrap_or_default();

    let bl_rx = section_rx("BlockList");
    let (block_list, shims) = extract_block(&text, &bl_rx)
        .map(|b| parse_block_list(b, config.subdivisions))
        .unwrap_or_default();

    let ba_rx = section_rx("BlockArray");
    let (blocks, parts) = extract_block(&text, &ba_rx)
        .map(|block| {
            let source_offset = block.as_ptr() as usize - text.as_ptr() as usize;
            parse_block_array(block, source_offset, &source_line_starts)
        })
        .unwrap_or_default();

    let mut lyrics = Vec::new();
    let lyrics_rx = section_rx("Lyrics");
    if let Some(block) = extract_block(&text, &lyrics_rx) {
        let block_source_offset = block.as_ptr() as usize - text.as_ptr() as usize;
        let frame_len = 4.0 * config.subdivisions as f64; // Assumes 4/4 time
        let mut row_offset = 0.0;
        let mut block_offset = 0usize;

        // Process the Lyrics block line by line
        for raw_line in block.split('\n') {
            let line = raw_line.trim();
            if line.is_empty() {
                block_offset += raw_line.len() + 1;
                continue;
            }
            // A semicolon means "skip one measure"
            if line == ";" {
                row_offset += frame_len;
                block_offset += raw_line.len() + 1;
                continue;
            }

            // Parse the events on this line
            let events = parse_part_events(
                raw_line,
                config.subdivisions,
                4,
                row_offset,
                block_source_offset + block_offset,
                &source_line_starts,
            );
            let mut segments = Vec::new();
            for ev in events {
                if let Some(text) = ev.lyric {
                    segments.push(LyricSegment {
                        row: ev.row, // ev.row already has row_offset added by parse_part_events
                        text,
                    });
                }
            }

            // If the line had lyrics, save it as an active line
            if !segments.is_empty() {
                lyrics.push(LyricEvent {
                    start_row: row_offset,
                    end_row: row_offset + frame_len,
                    segments,
                });
            }

            // Move to the next measure
            row_offset += frame_len;
            block_offset += raw_line.len() + 1;
        }
    }

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
        eprintln!(
            "ts: {} parts parsed, {} shims, {} fallbacks",
            parts.len(),
            shims.len(),
            fallbacks.len()
        );
    }

    Ok(HalotextFile {
        config,
        tempo_events: tempo,
        block_list,
        shims,
        blocks,
        parts,
        notes,
        lyrics,
        fallbacks,
    })
}

pub fn parse_file(path: &Path) -> Result<HalotextFile> {
    let text = fs::read_to_string(path)
        .map_err(|e| anyhow!("reading {}: {}", path.display(), e))?;
    parse_str(&text)
}
