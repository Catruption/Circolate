//! BlockList and BlockArray parsing.

use std::collections::HashMap;

use crate::spec::{
    FIELD_DURATION, FIELD_PAN, FIELD_SOURCE, FIELD_VOLUME, PART_HEADER_SIGIL,
};

use super::body::{
    flatten_body_items, make_part, parse_body, register_sub_blocks, BodyItem, Cursor, PartArgs,
};
use super::call_rx;
use super::events::parse_source_args;
use super::match_pair;
use super::next_char_boundary;
use super::parse_f;
use super::shim::{
    find_shim_close, parse_shim_event_with_default, parse_shim_waypoint_into,
};
use super::source_rx;
use super::split_fields;
use crate::model::halotext::{
    BlockCall, CallOverrides, HalotextEvent, HalotextPart, MeasureSelection, ShimEvent,
};

type BlockMap = HashMap<String, Vec<String>>;
type PartMap = HashMap<String, HalotextPart>;

pub(crate) fn parse_block_list(body: &str, subs: u32) -> (Vec<Vec<BlockCall>>, Vec<ShimEvent>) {
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

/// Parse one `@<...>` group starting at `at` (which must point at the
/// `@`), plus any trailing `&<...>` waypoints.
///
/// A single group may contain multiple comma-separated targets:
///   `@<ƒEQ2(), ƒReverb2()>`           — two shims, no universal fields
///   `@<§1/1, ~8, ƒEQ2(), ƒReverb2()>` — two shims sharing start/length
/// Universal fields (anything but `ƒ`) apply to every target in the group.
/// An empty group, `@<>`, is an explicit bypass.
///
/// `default_mix` of None means "wet if attached to a call, dry otherwise".
///
/// Returns `(shims, next_pos, is_bypass)`.
pub(crate) fn parse_shim_group(
    text: &str,
    at: usize,
    line_idx: usize,
    subs: u32,
    attached: Option<usize>,
    default_mix: Option<f64>,
) -> (Vec<ShimEvent>, usize, bool) {
    let bytes = text.as_bytes();
    let inner_start = at + 2;
    let Some(close) = find_shim_close(text, inner_start) else {
        return (Vec::new(), text.len(), false);
    };
    let inner = &text[inner_start..close];
    let mut pos = close + 1;

    if inner.trim().is_empty() {
        return (Vec::new(), pos, true);
    }

    let mut universal: Vec<String> = Vec::new();
    let mut targets: Vec<String> = Vec::new();
    for f in split_fields(inner) {
        if f.starts_with(FIELD_SOURCE) {
            targets.push(f);
        } else if !f.is_empty() {
            universal.push(f);
        }
    }

    let n = targets.len();
    let dm = default_mix.unwrap_or(if attached.is_some() { 1.0 } else { 0.0 });
    let mut out = Vec::new();

    for (i, tgt) in targets.into_iter().enumerate() {
        let synthesized = if universal.is_empty() {
            tgt
        } else {
            format!("{}, {}", universal.join(", "), tgt)
        };
        if let Some(mut shim) =
            parse_shim_event_with_default(&synthesized, line_idx, subs, attached, dm)
            {
                // Waypoints attach to the final shim in the group.
                if i == n - 1 {
                    while pos < bytes.len() && text[pos..].starts_with("&<") {
                        let wp_start = pos + 2;
                        let Some(wp_close) = find_shim_close(text, wp_start) else { break };
                        parse_shim_waypoint_into(&text[wp_start..wp_close], &mut shim, subs);
                        pos = wp_close + 1;
                    }
                }
                out.push(shim);
            }
    }

    (out, pos, false)
}

/// Parse one BlockList line into calls and any shims on it.  Each shim
/// is attached to the most recently parsed call (or None for standalone
/// shims at the start of a line).  `@<>` after a call marks that call
/// as bypassing the part's declaration chain.
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
            calls.push(BlockCall { name, selections, bypass_shims: false });
            last_call = Some(calls.len() - 1);
            continue;
        }

        // @<...>  — a shim group (or `@<>` bypass)
        if text[pos..].starts_with("@<") {
            let (s, next, bypass) =
            parse_shim_group(text, pos, line_idx, subs, last_call, None);
            if bypass {
                if let Some(ci) = last_call {
                    calls[ci].bypass_shims = true;
                }
            }
            shims.extend(s);
            pos = next;
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

pub(crate) fn parse_block_array(
    body: &str,
    source_offset: usize,
    source_line_starts: &[usize],
) -> (BlockMap, PartMap) {
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
        let mut preset: Option<i16> = None;
        let mut bank: Option<i16> = None;
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
                    let args = c.get(2).map(|m| m.as_str()).unwrap_or("");
                    let plus = c.get(3).map(|m| m.as_str()).unwrap_or("");
                    if !plus.is_empty() {
                        sample_offset = plus.parse().unwrap_or(0);
                    } else if !args.is_empty() {
                        let (pos_args, _named) = parse_source_args(args);
                        let lower = sample_path.to_lowercase();
                        if lower.ends_with(".sf2") {
                            match pos_args.len() {
                                1 => {
                                    preset = pos_args[0].parse().ok();
                                    bank = Some(0);
                                }
                                2 => {
                                    bank = pos_args[0].parse().ok();
                                    preset = pos_args[1].parse().ok();
                                }
                                _ => {}
                            }
                        } else {
                            if let Some(s) = pos_args.first() {
                                sample_offset = s.parse().unwrap_or(0);
                            }
                        }
                    }
                }
            } else if let Some(rest) = f.strip_prefix(FIELD_VOLUME) {
                default_vol = parse_f(rest.trim_start_matches('+'));
            } else if let Some(rest) = f.strip_prefix(FIELD_PAN) {
                default_pan = parse_f(rest);
            } else if let Some(rest) = f.strip_prefix(FIELD_DURATION) {
                default_dur = parse_f(rest.trim_start_matches('+'));
            }
        }

        // Declaration-header shim chain: `ƒX(<...>)@<ƒA(), ƒB()>[ ... ]`.
        // Always wet by default (there is no call to attach to).
        let mut decl_shims: Vec<ShimEvent> = Vec::new();
        while pos < bytes.len() && body[pos..].starts_with("@<") {
            let (s, next, _bypass) =
            parse_shim_group(body, pos, 0, subs_per_beat, None, Some(1.0));
            decl_shims.extend(s);
            pos = next;
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
        }

        let args = PartArgs {
            sample_path,
            sample_offset,
            preset,
            bank,
            src_bpm,
            beats_per_measure,
            subs_per_beat,
            default_vol,
            default_pan,
            default_dur,
            shims: decl_shims,
        };

        let mut measures: Vec<Vec<HalotextEvent>> = Vec::new();
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
                let evs = super::parse_part_events(
                    events_str,
                    sec_s,
                    sec_b,
                    0.0,
                    source_offset + bracket_open + 1,
                    source_line_starts,
                );
                measures.push(evs);
                section_lengths.push(sec_b as f64 * sec_s as f64);

                pos = bracket_close + 1;
                while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
            }
        } else {
            let mut cur = Cursor::new(body, pos, source_offset, source_line_starts);
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
