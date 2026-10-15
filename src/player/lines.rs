//! BlockList line resolution: expand each line into concrete measure
//! placements, and expand Tempo shims into tempo events.

use std::collections::HashMap;

use crate::sources::trackscript::{BlockCall, CallOverrides, ShimEvent, TrackscriptPart};

#[derive(Clone)]
pub(crate) struct LineCall {
    /// Formatted BlockList call, used instead of the block's original title tag.
    pub block_title: String,
    /// Resolved part name used for note/event lookup.
    pub name: String,
    pub pattern_measure_start: f64,
    pub pattern_measure_len: f64,
    pub line_offset: f64,
    pub overrides: Option<CallOverrides>,
}

pub(crate) struct LineInfo {
    pub parts: Vec<LineCall>,
    pub start_row: f64,
    pub min_start: i64,
    pub max_end: i64,
}


fn format_block_title(call: &BlockCall) -> String {
    let mut title = format!("ƒ{}(", call.name);
    for (i, selection) in call.selections.iter().enumerate() {
        if i > 0 {
            title.push(',');
        }
        title.push_str(&(selection.measure + 1).to_string());

        if let Some(overrides) = &selection.overrides {
            let mut fields = Vec::new();
            if let Some(vol) = overrides.vol {
                fields.push(format!("%{}", vol));
            }
            if let Some(pan) = overrides.pan {
                fields.push(format!("¶{}", pan));
            }
            if let Some(dur) = overrides.dur {
                fields.push(format!("~{}", dur));
            }
            if let Some(source) = &overrides.source {
                fields.push(format!("ƒ'{}'", source));
            }
            if !fields.is_empty() {
                title.push('<');
                title.push_str(&fields.join(", "));
                title.push('>');
            }
        }
    }
    title.push(')');
    title
}

pub(crate) struct ResolvedLine {
    pub calls: Vec<LineCall>,
    pub duration: f64,
}

pub(crate) fn tempo_shims_to_events(
    shims: &[ShimEvent],
    rows_per_measure: f64,
) -> Vec<(f64, f64, f64)> {
    let mut ordered: Vec<(f64, &ShimEvent)> = shims
        .iter()
        .filter(|s| s.target == "Tempo")
        .map(|s| (s.line as f64 * rows_per_measure + s.row, s))
        .collect();
    ordered.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out: Vec<(f64, f64, f64)> = Vec::new();

    for (base_row, shim) in ordered {
        out.retain(|(r, _, _)| *r < base_row);
        out.push((base_row, shim.start_value, 0.0));

        let count = shim.count.unwrap_or(1);
        let interval = if shim.interval_rows > 0.0 {
            shim.interval_rows
        } else {
            rows_per_measure
        };

        let mut value = shim.start_value;
        for i in 0..count {
            let row = base_row + (i as f64 + 1.0) * interval;
            match shim.op.as_str() {
                "+" => value += shim.amount,
                "-" => value -= shim.amount,
                "*" => value *= shim.amount,
                "±" => {
                    let dir = if i % 2 == 0 { -1.0 } else { 1.0 };
                    value = shim.start_value + dir * shim.amount;
                }
                _ => {}
            }
            out.push((row, value, 0.0));
        }
    }

    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn resolve_call(
    call: &BlockCall,
    blocks: &HashMap<String, Vec<String>>,
    parts: &HashMap<String, TrackscriptPart>,
    rows_per_measure: f64,
) -> Vec<LineCall> {
    let part_names: Vec<String> = blocks
        .get(&call.name)
        .cloned()
        .unwrap_or_else(|| vec![call.name.clone()]);

    let mut out: Vec<LineCall> = Vec::new();

    if part_names.len() == 1 {
        let name = &part_names[0];
        let Some(part) = parts.get(name) else { return out };
        let sections: Vec<f64> = if part.section_lengths.is_empty() {
            vec![rows_per_measure]
        } else {
            part.section_lengths.clone()
        };

        let chosen: Vec<(usize, Option<CallOverrides>)> = if call.selections.is_empty() {
            (0..sections.len()).map(|m| (m, None)).collect()
        } else {
            call.selections
                .iter()
                .map(|s| (s.measure, s.overrides.clone()))
                .collect()
        };

        let mut offset = 0.0;
        for (m_idx, overrides) in chosen {
            if m_idx >= sections.len() {
                continue;
            }
            let measure_start: f64 = sections[..m_idx].iter().sum();
            let measure_len = sections[m_idx];
            out.push(LineCall {
                block_title: format_block_title(call),
                name: name.clone(),
                pattern_measure_start: measure_start,
                pattern_measure_len: measure_len,
                line_offset: offset,
                overrides,
            });
            offset += measure_len;
        }
    } else {
        for name in &part_names {
            let Some(part) = parts.get(name) else { continue };
            let sections: Vec<f64> = if part.section_lengths.is_empty() {
                vec![rows_per_measure]
            } else {
                part.section_lengths.clone()
            };
            let mut offset = 0.0;
            for (m_idx, measure_len) in sections.iter().enumerate() {
                let measure_start: f64 = sections[..m_idx].iter().sum();
                out.push(LineCall {
                    block_title: format_block_title(call),
                    name: name.clone(),
                    pattern_measure_start: measure_start,
                    pattern_measure_len: *measure_len,
                    line_offset: offset,
                    overrides: None,
                });
                offset += measure_len;
            }
        }
    }

    out
}

pub(crate) fn resolve_line(
    line: &[BlockCall],
    blocks: &HashMap<String, Vec<String>>,
    parts: &HashMap<String, TrackscriptPart>,
    rows_per_measure: f64,
) -> ResolvedLine {
    let mut calls: Vec<LineCall> = Vec::new();
    let mut duration: f64 = rows_per_measure;

    for call in line {
        let this_call = resolve_call(call, blocks, parts, rows_per_measure);
        let this_duration: f64 = this_call.iter().map(|c| c.pattern_measure_len).sum();
        if this_duration > duration {
            duration = this_duration;
        }
        calls.extend(this_call);
    }

    ResolvedLine { calls, duration }
}