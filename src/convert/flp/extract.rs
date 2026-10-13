// src/convert/flp/extract.rs
//! Fidelity sidecar extraction: per-channel metadata, playlist placements,
//! and the mixer insert chain.

use std::collections::BTreeMap;

use crate::convert::{identifier, AuxFile};
use crate::fallbacks::FallbackTable;
use crate::fidelity::{
    ChannelFidelity, EnvelopeFidelity, Fidelity, InsertFidelity, InsertSlotFidelity,
    LevelsFidelity, PlaylistFidelity, TrackingFidelity, FIDELITY_VERSION,
};
use crate::plugins::PluginIndex;
use crate::sources::flp as flp_src;

use super::resolve::{resolve_plugin, Resolved};

// ── per-channel + playlist metadata ────────────────────────────────────

/// Build the fidelity sidecar from the parsed FLP.
///
/// Uses `serde_json::to_value(chan)` once per channel as an intermediate,
/// then extracts each field by name with `.get()`.  That way a malformed
/// sub-object (`levels`, `envelopes`, ...) produces a partial entry
/// instead of silently dropping the whole channel.
pub(super) fn build_fidelity(prj: &flp_src::Project) -> Fidelity {
    let mut f = Fidelity {
        version: FIDELITY_VERSION,
        ..Default::default()
    };

    let debug = std::env::var_os("FIDELITY_DEBUG").is_some();
    let mut dumped = false;

    for chan in &prj.channels {
        let Ok(v) = serde_json::to_value(chan) else {
            eprintln!("fidelity: to_value failed for channel {}", chan.iid);
            continue;
        };
        if debug && !dumped {
            eprintln!(
                "fidelity: first channel value: {}",
                serde_json::to_string_pretty(&v).unwrap_or_default()
            );
            dumped = true;
        }
        let cf = channel_fidelity_from_value(chan.iid, &v);
        f.channels.insert(cf.iid.to_string(), cf);
    }

    for item in &prj.playlist {
        let Ok(v) = serde_json::to_value(item) else {
            continue;
        };
        f.playlist.push(playlist_fidelity_from_value(&v));
    }

    f
}

fn channel_fidelity_from_value(fallback_iid: u16, v: &serde_json::Value) -> ChannelFidelity {
    let iid = v
    .get("iid")
    .and_then(|x| x.as_u64())
    .unwrap_or(fallback_iid as u64) as u16;

    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();

    let channel_type = v
    .get("type")
    .or_else(|| v.get("channel_type"))
    .and_then(|x| x.as_str())
    .unwrap_or("")
    .to_string();

    let sample_path = v
    .get("sample_path")
    .and_then(|x| x.as_str())
    .unwrap_or("")
    .to_string();

    let levels = v
    .get("levels")
    .cloned()
    .and_then(|x| serde_json::from_value::<LevelsFidelity>(x).ok());
    let level_adjusts = v
    .get("level_adjusts")
    .cloned()
    .and_then(|x| serde_json::from_value::<LevelsFidelity>(x).ok());

    let envelopes: Vec<EnvelopeFidelity> = v
    .get("envelopes")
    .and_then(|x| x.as_array())
    .map(|arr| {
        arr.iter()
        .filter_map(|e| serde_json::from_value::<EnvelopeFidelity>(e.clone()).ok())
        .collect()
    })
    .unwrap_or_default();

    let tracking: Vec<TrackingFidelity> = v
    .get("tracking")
    .and_then(|x| x.as_array())
    .map(|arr| {
        arr.iter()
        .filter_map(|t| serde_json::from_value::<TrackingFidelity>(t.clone()).ok())
        .collect()
    })
    .unwrap_or_default();

    let num = |key: &str| v.get(key).and_then(|x| x.as_f64());

    ChannelFidelity {
        iid,
        name,
        channel_type,
        sample_path,
        levels,
        level_adjusts,
        envelopes,
        tracking,
        sampler_flags: num("sampler_flags"),
        root_note: num("root_note"),
        fine_tune: num("fine_tune"),
    }
}

fn playlist_fidelity_from_value(v: &serde_json::Value) -> PlaylistFidelity {
    let num = |key: &str| v.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0);

    let kind = v
    .get("kind")
    .and_then(|x| x.as_str())
    .unwrap_or("")
    .to_string();

    let ref_id = v
    .get("ref")
    .or_else(|| v.get("ref_id"))
    .and_then(|x| x.as_f64())
    .unwrap_or(0.0);

    let start_offset_ms = v
    .get("start_offset_ms")
    .and_then(|x| x.as_f64())
    .unwrap_or(-1.0);
    let end_offset_ms = v
    .get("end_offset_ms")
    .and_then(|x| x.as_f64())
    .unwrap_or(-1.0);

    PlaylistFidelity {
        position: num("position"),
        length: num("length"),
        track: num("track"),
        kind,
        ref_id,
        clip_start_ticks: num("clip_start_ticks"),
        start_offset_ms,
        end_offset_ms,
    }
}

// ── mixer insert chains ────────────────────────────────────────────────

/// Extract every insert slot with saved state.  Writes `.state` blobs
/// (and `.instrument` sidecars for VST slots) into `aux_files`, and
/// returns the manifest map.  Fruity built-ins (Parametric EQ 2, Soft
/// Clipper, Blood Overdrive, Delay 2, Convolver, Emphasis) have no
/// vst_path and stay state-only.
pub(super) fn extract_inserts(
    prj: &flp_src::Project,
    plugin_index: &PluginIndex,
    fallback_table: Option<&FallbackTable>,
    aux_files: &mut Vec<AuxFile>,
) -> BTreeMap<String, InsertFidelity> {
    let mut out: BTreeMap<String, InsertFidelity> = BTreeMap::new();

    for (insert_index, ins) in &prj.inserts {
        let mut slots: Vec<InsertSlotFidelity> = Vec::new();
        for (slot_i, plugin) in ins.plugins.iter().enumerate() {
            let display = if !plugin.internal_name.is_empty() {
                plugin.internal_name.clone()
            } else {
                plugin.display_name.clone()
            };
            if display.is_empty() && plugin.state.is_empty() {
                continue;
            }

            let base_raw = format!(
                "insert{:02}_slot{:02}_{}",
                insert_index, slot_i, display
            );
            let base = identifier(&base_raw);

            let state_rel = if !plugin.state.is_empty() {
                let rel = format!("inserts/{}.state", base);
                aux_files.push(AuxFile {
                    rel_path: rel.clone(),
                    bytes: plugin.state.clone(),
                });
                Some(rel)
            } else {
                None
            };

            let lower = plugin.vst_path.to_ascii_lowercase();
            let is_vst = !plugin.vst_path.is_empty()
                && (lower.ends_with(".vst3")
                    || lower.ends_with(".vst")
                    || lower.ends_with(".dll"));

            let mut instrument_rel: Option<String> = None;
            let mut fallback = false;

            if is_vst {
                let resolved = resolve_plugin(plugin, plugin_index, fallback_table);
                fallback = matches!(resolved, Resolved::Fallback { .. });

                let mut lines = vec![
                    format!("Instrument {base}"),
                    "{".to_string(),
                    "    Kind: vst3;".to_string(),
                ];
                match &resolved {
                    Resolved::Exact { name, path } => {
                        lines.push(format!("    Plugin: '{}';", name));
                        lines.push(format!("    Path: '{}';", path.display()));
                        if state_rel.is_some() {
                            lines.push(format!("    State: '{}.state';", base));
                        }
                    }
                    Resolved::Fallback { name, path, replaces, gain } => {
                        lines.push(format!("    Plugin: '{}';", name));
                        if let Some(p) = path {
                            lines.push(format!("    Path: '{}';", p.display()));
                        } else {
                            lines.push("    Unresolved: true;".to_string());
                        }
                        lines.push(format!("    Replaces: '{}';", replaces));
                        lines.push("    Fallback: true;".to_string());
                        lines.push(format!("    Gain: {};", gain));
                    }
                    Resolved::Unresolved { name } => {
                        lines.push(format!("    Plugin: '{}';", name));
                        lines.push("    Unresolved: true;".to_string());
                    }
                }
                if !plugin.internal_name.is_empty() {
                    lines.push(format!("    OriginalPlugin: '{}';", plugin.internal_name));
                }
                lines.push(format!("    Insert: {};", insert_index));
                lines.push(format!("    Slot: {};", slot_i));
                lines.push("}".to_string());

                let inst_rel = format!("inserts/{}.instrument", base);
                aux_files.push(AuxFile {
                    rel_path: inst_rel.clone(),
                    bytes: (lines.join("\n") + "\n").into_bytes(),
                });
                instrument_rel = Some(inst_rel);
            }

            slots.push(InsertSlotFidelity {
                slot: slot_i as u32,
                plugin: display,
                internal_name: plugin.internal_name.clone(),
                vst_path: plugin.vst_path.clone(),
                instrument: instrument_rel,
                state: state_rel,
                fallback,
            });
        }
        if slots.is_empty() {
            continue;
        }
        out.insert(
            insert_index.to_string(),
            InsertFidelity {
                name: ins.name.clone(),
                slots,
            },
        );
    }

    out
}