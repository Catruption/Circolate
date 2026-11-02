//! Part body parsing: measures, sub-blocks, definitions.

use std::collections::HashMap;

use crate::spec::FIELD_SOURCE;

use super::events::parse_part_events;
use super::types::{HalotextEvent, HalotextPart};

pub(crate) enum BodyItem {
    Measure(Vec<HalotextEvent>),
    SubBlock { name: String, items: Vec<BodyItem> },
}

pub(crate) struct Cursor<'a> {
    pub(crate) s: &'a str,
    pub(crate) pos: usize,
    pub(crate) source_offset: usize,
    pub(crate) source_line_starts: &'a [usize],
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(
        s: &'a str,
        pos: usize,
        source_offset: usize,
        source_line_starts: &'a [usize],
    ) -> Self {
        Cursor { s, pos, source_offset, source_line_starts }
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

fn parse_measure(c: &mut Cursor, subs: u32, beats: u32) -> Vec<HalotextEvent> {
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
        if let Some(ev) = parse_part_events(
            ev_str,
            subs,
            beats,
            0.0,
            c.source_offset + start,
            c.source_line_starts,
        ).into_iter().next() {
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

pub(crate) fn parse_body(c: &mut Cursor, subs: u32, beats: u32) -> Vec<BodyItem> {
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

pub(crate) fn flatten_body_items(items: &[BodyItem], out: &mut Vec<Vec<HalotextEvent>>) {
    for item in items {
        match item {
            BodyItem::Measure(events) => out.push(events.clone()),
            BodyItem::SubBlock { items, .. } => flatten_body_items(items, out),
        }
    }
}

pub(crate) fn register_sub_blocks(
    items: &[BodyItem],
    args: &PartArgs,
    parts: &mut HashMap<String, HalotextPart>,
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

pub(crate) struct PartArgs {
    pub(crate) sample_path: String,
    pub(crate) sample_offset: u32,
    pub(crate) preset: Option<i16>,
    pub(crate) bank: Option<i16>,
    pub(crate) src_bpm: Option<f64>,
    pub(crate) beats_per_measure: u32,
    pub(crate) subs_per_beat: u32,
    pub(crate) default_vol: Option<f64>,
    pub(crate) default_pan: Option<f64>,
    pub(crate) default_dur: Option<f64>,
}

pub(crate) fn make_part(
    name: String,
    args: &PartArgs,
    measures: Vec<Vec<HalotextEvent>>,
    section_lengths: Vec<f64>,
) -> HalotextPart {
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
    HalotextPart {
        name,
        sample_path: args.sample_path.clone(),
        beats_per_measure: args.beats_per_measure,
        subs_per_beat: args.subs_per_beat,
        len_beats: None,
        sample_offset: args.sample_offset,
        preset: args.preset,
        bank: args.bank,
        src_bpm: args.src_bpm,
        events,
        measures,
        section_lengths,
        default_vol: args.default_vol,
        default_pan: args.default_pan,
        default_dur: args.default_dur,
    }
}