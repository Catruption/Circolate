// src/convert/it/report.rs
//! Human-readable report, semantics JSON.

use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

use super::helpers::*;
use super::render::build_sections;
use super::sim::{note_name_from_it, Converter};
use crate::sources::it::Module;

pub(super) fn make_report(
    conv: &mut Converter,
    source_name: &str,
    sample_files: &HashMap<u32, String>,
) -> String {
    let m = conv.m;

    let runs = build_sections(conv);

    let unique_part_count = 0usize;
    let pattern_placement_count: usize = conv.voices.len();

    let mut r: Vec<String> = Vec::new();

    r.push(format!("it2trackscript report for {}", source_name));
    r.push(format!("title: {}", m.title));
    r.push(format!("source archive: source/{} (byte-exact)", source_name));
    r.push(format!(
        "orders: {}   patterns: {}   rows played: {}",
        m.orders.len(),
        m.patterns.len(),
        conv.total_rows
    ));
    r.push(format!(
        "instruments: {}   samples: {}",
        m.instruments.len(),
        m.samples.len()
    ));

    let shown: Vec<String> = runs
        .iter()
        .take(8)
        .map(|x| format!("{} BPM", fmt(x.2, 2)))
        .collect();
    r.push(format!(
        "timing sections: {} ({}{})",
        runs.len(),
        shown.join(", "),
        if runs.len() > 8 { " ..." } else { "" }
    ));

    r.push(format!(
        "notes converted: {}   waypoints: {}",
        conv.voices.len(),
        conv.voices.iter().map(|v| v.points.len()).sum::<usize>()
    ));

    r.push(String::new());

    r.push("Trackscript structure:".into());
    r.push(format!("  reusable pattern definitions: {}", unique_part_count));
    r.push(format!(
        "  pattern placements in BlockList: {}",
        pattern_placement_count
    ));

    if let Some((oi, row, _)) = conv.loop_info {
        r.push(format!("song loops back to order {} row {}", oi, row));
    }

    r.push(String::new());
    r.push("approximations / things not converted:".into());

    if conv.warn.is_empty() {
        r.push("  (none)".into());
    }

    let mut warns = conv.warn.clone();
    warns.sort_by(|a, b| b.1.cmp(&a.1));
    for (k, n) in &warns {
        let extra = conv
            .warn_detail
            .get(k)
            .map(|d| format!("  -- {}", d))
            .unwrap_or_default();
        r.push(format!("  {:5} x {}{}", n, k, extra));
    }

    let mut envs: Vec<String> = Vec::new();

    for (i, ins) in m.instruments.iter().enumerate() {
        let used: Vec<&str> = ["volume", "pan", "pitch"]
            .into_iter()
            .filter(|n| ins.envelopes.get(*n).map_or(false, |e| e.enabled))
            .collect();

        if !used.is_empty() && ins.keymap.iter().any(|&(_, s)| s != 0) {
            let statuses: Vec<String> = used
                .iter()
                .map(|&name| {
                    let envelope = &ins.envelopes[name];
                    if name == "volume"
                        && !envelope.points.is_empty()
                        && !envelope.loop_enabled
                        && !envelope.sustain_enabled
                    {
                        "volume converted to approximate automation".to_string()
                    } else {
                        format!("{} playback not converted", name)
                    }
                })
                .collect();
            envs.push(format!(
                "  instrument {:02} {}: {}; source data retained in samples.json",
                i + 1,
                py_repr(&ins.name),
                statuses.join("; ")
            ));
        }
    }

    if !envs.is_empty() {
        r.push(String::new());
        r.push(
            "instrument envelope playback status (all source envelope data retained in samples.json):"
                .into(),
        );
        r.extend(envs);
    }

    let _ = sample_files;

    r.join("\n") + "\n"
}

// ── semantics JSON ─────────────────────────────────────────────────────

fn decoded_row_events(module: &Module) -> Vec<Value> {
    let mut out = Vec::new();
    for (pat_i, grid) in module.patterns.iter().enumerate() {
        for (row_i, cells) in grid.iter().enumerate() {
            let mut events = Vec::new();
            for (ch, raw) in cells.iter().enumerate() {
                let Some(c) = super::sim::read_cell(raw) else { continue };
                if c.note.is_none() && c.inst.is_none() && c.volc.is_none() && c.cmd == 0 {
                    continue;
                }
                let effect = if (1..=26).contains(&c.cmd) {
                    Some(((64 + c.cmd) as char).to_string())
                } else {
                    None
                };
                events.push(json!({
                    "channel": ch,
                    "note_raw": c.note,
                    "note": note_name_from_it(c.note),
                    "instrument": c.inst,
                    "volume_column": c.volc,
                    "effect": effect,
                    "effect_raw": c.cmd,
                    "parameter": c.par,
                }));
            }
            if !events.is_empty() {
                out.push(json!({ "pattern": pat_i, "row": row_i, "events": events }));
            }
        }
    }
    out
}

fn voice_semantics(conv: &Converter) -> Vec<Value> {
    conv.voices
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let pitch_hz = v.init.get("pitch").copied();
            let pitch = pitch_hz
                .filter(|&h| h > 0.0)
                .map(|h| round_even(60.0 + 12.0 * (h / C5_HZ).log2()) as i64);
            json!({
                "voice": i,
                "channel": v.ch,
                "start_row": v.start,
                "end_row": v.end,
                "end_reason": if v.natural_rows.is_some() && v.end.is_some() { "natural_end" } else { "channel_end" },
                "sample": v.sample,
                "note": note_name_from_it(Some(v.note)),
                "pitch": pitch,
                "pitch_hz": pitch_hz,
                "volume": v.init.get("vol"),
                "pan": v.init.get("pan"),
                "offset": v.offset,
                "fade": v.fade.map(|(a, b)| vec![a, b]),
                "waypoints": v.points.iter().map(|&(pt, prop, val, dur)| json!({
                    "row": pt, "property": prop, "value": val, "duration": dur,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn channel_semantics(conv: &Converter) -> Vec<Value> {
    conv.ch
        .iter()
        .enumerate()
        .map(|(i, st)| {
            json!({
                "channel": i,
                "instrument": st.inst,
                "volume": st.vol,
                "channel_volume": st.chanvol,
                "pan": st.pan,
                "pitch": st.pitch,
                "global_scale": st.gscale,
                "sample_offset": st.mem_o,
                "vibrato_speed": st.vib_spd,
                "vibrato_depth": st.vib_dep,
                "active_voice": st.voice,
            })
        })
        .collect()
}

pub(super) fn build_semantics_json(
    module: &Module,
    conv: &Converter,
    source_name: &str,
) -> Result<String> {
    let tempo_events: Vec<Value> = conv
        .rowlog
        .iter()
        .filter(|r| r.1 == 0 && r.2 == 0 && r.3 == 0)
        .map(|r| json!({ "row": r.0, "bpm": r.4 }))
        .collect();

    let data = json!({
        "format": "trackscript-semantics",
        "version": 1,
        "source": source_name,
        "converter": "it2trackscript",
        "source_archive": format!("source/{}", source_name),
        "module": {
            "title": module.title,
            "initial_speed": module.speed,
            "initial_tempo": module.tempo,
            "global_volume": module.gv,
            "mix_volume": module.mv,
            "stereo_separation": module.sep,
            "pitch_wheel_depth": module.pwd,
            "flags": module.flags,
            "special_flags": module.special,
            "linear_pitch": module.linear,
            "old_effects": module.old_effects,
            "orders_raw": module.orders,
            "channel_pan_raw": module.chnpan,
            "channel_volume_raw": module.chnvol,
            "message": module.message,
            "orders": module.orders.len(),
            "patterns": module.patterns.len(),
            "instruments": module.instruments.len(),
            "samples": module.samples.len(),
        },
        "it_decode": {
            "pattern_count": module.patterns.len(),
            "instrument_count": module.instruments.len(),
            "sample_count": module.samples.len(),
            "rows_with_events": decoded_row_events(module),
        },
        "simulation": {
            "total_rows": conv.total_rows,
            "rows_per_beat": ROWS_PER_BEAT,
            "rows_per_measure": ROWS_PER_BEAT * 4,
            "initial_bpm": conv.bpm(),
            "final_speed": conv.speed,
            "final_tempo": conv.tempo,
            "final_global_volume": conv.gvol,
            "tempo_events": tempo_events,
            "channels": channel_semantics(conv),
            "executed_rows": conv.executed_rows,
            "voices": voice_semantics(conv),
        },
        "effects": {
            "seen": {},
            "represented": {},
            "approximated": {},
            "dropped": {},
        },
        "warnings": conv.warn.iter().cloned().collect::<BTreeMap<String, u32>>(),
    });

    Ok(serde_json::to_string_pretty(&data)?)
}