//! The heavy lifting for loading a project from an AssetSource.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::asset::{AssetSource, DirSource, ENTRY_KEY};
use crate::audio_decode::decode_audio;
use crate::deps::index::PluginIndex;
use crate::fidelity::{Fidelity, FIDELITY_FILENAME};
use crate::deps::requires::{self, Requires};
use crate::formats::halotext::parse_str;
use crate::hosting::chip::patch::Patch;
use crate::hosting::chip::patch::parse as parse_patch;
use crate::timing::TempoMap;

use super::super::cache::{fingerprint, hash_part, Cache};
use super::super::inserts;
use super::super::lines::{resolve_line, tempo_shims_to_events, LineInfo};
use super::super::types::{
    build_sf2_instrument, MultiSampleInstrument, Sf2EngineSpec, Sf2Instrument,
    Sf2InstrumentOverrides, VstInstrument, Sample, Sf2VoiceEnvelope,
};
use super::assets::{extract_embedded_plugins, materialize_sf2_file, materialize_sf2_reference};
use super::{LyricLine, LyricSegment, Project, VgmInstrument};

const TAIL_SECONDS: f64 = 0.05;
const VST_TRIM: f32 = 0.6;
const FALLBACK_VST_TRIM: f32 = 0.7;

const PATCH_IID_BASE: u16 = 20_000;

fn is_chip_playable_part(sample_path: &str) -> bool {
    sample_path.to_lowercase().contains("vgm") || sample_path.ends_with(".patch")
}

#[derive(Deserialize, Debug)]
struct SampleEntry {
    id: u32,
    file: Option<String>,
    #[serde(default)]
    loops: Loops,
}

#[derive(Deserialize, Debug, Default)]
struct Loops {
    main: Option<Loop>,
}

#[derive(Deserialize, Debug)]
struct Loop {
    #[serde(default)]
    start: u32,
    #[serde(default)]
    end: u32,
}

#[derive(Deserialize, Debug)]
struct SampleLibrary {
    #[serde(default)]
    samples: Vec<SampleEntry>,
}

fn vgm_seconds(bytes: &[u8]) -> Option<f64> {
    if bytes.len() < 0x1C || &bytes[0..4] != b"Vgm " {
        return None;
    }
    let n = u32::from_le_bytes([bytes[0x18], bytes[0x19], bytes[0x1A], bytes[0x1B]]);
    if n > 0 { Some(n as f64 / 44100.0) } else { None }
}

pub(crate) fn load_project_from_source(
    source: &dyn AssetSource,
    sample_rate: u32,
    use_cache: bool,
    _quiet: bool,
    title_hint: Option<&str>,
) -> Result<(Arc<Project>, Cache)> {
    let ts_text = source
    .read_text(ENTRY_KEY)
    .context("reading halotext")?
    .ok_or_else(|| anyhow::anyhow!("source has no halotext: {}", source.describe()))?;
    let parsed = parse_str(&ts_text)?;
    let cfg = &parsed.config;

    let source_dir = PathBuf::new();
    let cover_path = super::assets::find_cover_path(source)
    .or_else(|| super::assets::fallback_cover_path(source));

    let fidelity: Option<Fidelity> = match source.read_text(FIDELITY_FILENAME)? {
        Some(text) => match Fidelity::from_json(&text) {
            Ok(f) => Some(f),
            Err(_) => None,
        },
        None => None,
    };

    let embedded_plugins: HashMap<String, PathBuf> = match extract_embedded_plugins(source) {
        Ok(m) => m,
        Err(e) => {
            crate::log_event!("player.embed.extract_failed", err: format!("{:#}", e));
            HashMap::new()
        }
    };

    let mut samples: HashMap<u32, Arc<Sample>> = HashMap::new();
    let mut fps: HashMap<u32, u64> = HashMap::new();
    let mut path_to_id: HashMap<String, u32> = HashMap::new();

    match source.read_text("samples.json")? {
        Some(text) if !text.trim().is_empty() => {
            let lib: SampleLibrary = serde_json::from_str(&text).context("parsing samples.json")?;
            for s in lib.samples {
                let Some(rel) = s.file else { continue };
                let Some(bytes) = source.read_bytes(&rel)? else { continue };
                let decoded = match decode_audio(&bytes, &rel) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let rate = decoded.rate;
                let stereo = decoded.channels_count() == 2;
                let mut chans = decoded.channels;
                let right = if stereo { chans.pop().unwrap() } else { Vec::new() };
                let left = chans.pop().unwrap_or_default();
                let loop_pts = s.loops.main.filter(|l| l.end > l.start)
                .map(|l| (l.start as i64, l.end as i64));
                let smp = Sample { left, right, stereo, rate, loop_pts };
                fps.insert(s.id, fingerprint(&smp));
                samples.insert(s.id, Arc::new(smp));

                // Bare-filename alias so `ƒ'X.wav'` resolves.
                if let Some(base) = Path::new(&rel).file_name().and_then(|f| f.to_str()) {
                    if base != rel.as_str() {
                        path_to_id.entry(base.to_string()).or_insert(s.id);
                    }
                }

                if let Some(stripped) = rel.strip_prefix("./") {
                    path_to_id.insert(stripped.to_string(), s.id);
                }
                path_to_id.insert(rel, s.id);
            }
        }
        _ => {}
    }

    let mut multi_sample_instruments: HashMap<String, Arc<MultiSampleInstrument>> = HashMap::new();
    if let Some(fid) = &fidelity {
        for (_iid_str, ms) in &fid.multi_sample {
            if ms.files.is_empty() { continue; }
            if let Some(inst) = MultiSampleInstrument::new(&ms.files, &ms.root_keys, &path_to_id) {
                let primary = ms.files[0].clone();
                multi_sample_instruments.insert(primary, Arc::new(inst));
            }
        }
    }

    let mut sf2_instruments: HashMap<String, Arc<Sf2Instrument>> = HashMap::new();
    let mut sf2_engine_specs: HashMap<String, Sf2EngineSpec> = HashMap::new();
    let mut vst_instruments: HashMap<String, Arc<VstInstrument>> = HashMap::new();
    let mut vgm_instruments: HashMap<String, Arc<VgmInstrument>> = HashMap::new();

    // (sidecar_stem, plugin_name) captured during instrument loop,
    // consumed after the loop so aliases point at the final Arc.
    let mut vst_aliases: Vec<(String, String)> = Vec::new();

    let mut next_sf2_id: u32 = 10_000;
    let mut vst_index: Option<PluginIndex> = None;

    let has_patch_parts = parsed.parts.values()
    .any(|p| p.sample_path.ends_with(".patch"));

    let vgm_bytes_opt = source.read_bytes("audio.vgm").ok().flatten();
    let vgm_dac = vgm_bytes_opt.as_deref()
    .and_then(crate::hosting::chip::dac::extract_dac_track);
    if let Some(vgm_bytes) = vgm_bytes_opt.as_ref() {
        for part in parsed.parts.values() {
            if part.sample_path.to_lowercase().contains("vgm") {
                vgm_instruments.insert(
                    part.sample_path.clone(),
                                       Arc::new(VgmInstrument {
                                           iid: 9999,
                                           file_bytes: vgm_bytes.clone(),
                                       }),
                );
            }
        }
        if vgm_instruments.is_empty() && !has_patch_parts {
            for part in parsed.parts.values() {
                vgm_instruments.insert(
                    part.sample_path.clone(),
                                       Arc::new(VgmInstrument {
                                           iid: 9999,
                                           file_bytes: vgm_bytes.clone(),
                                       }),
                );
            }
        }
    }

    let mut patch_instruments: HashMap<String, Patch> = HashMap::new();
    let mut patch_iids: HashMap<String, u16> = HashMap::new();

    if has_patch_parts {
        for part in parsed.parts.values() {
            if !part.sample_path.ends_with(".patch") { continue; }
            if patch_instruments.contains_key(&part.sample_path) { continue; }

            let Some(text) = source.read_text(&part.sample_path)? else {
                crate::log_event!(
                    "player.patch.missing",
                    path: part.sample_path.clone(),
                );
                continue;
            };
            match parse_patch(&text) {
                Ok(patch) => {
                    let iid: u16 = if patch.requires.iter()
                    .any(|r| r.capability == "fm.operator" || r.capability == "sample.pcm") { 101 } else { 100 };
                    patch_iids.insert(part.sample_path.clone(), iid);
                    patch_instruments.insert(part.sample_path.clone(), patch);
                }
                Err(e) => {
                    crate::log_event!(
                        "player.patch.parse_failed",
                        path: part.sample_path.clone(),
                                      err: e,
                    );
                }
            }
        }
        let psg_n = patch_iids.values().filter(|i| **i == 100).count();
        let fm_n = patch_iids.values().filter(|i| **i == 101).count();
        eprintln!(
            "loaded {} chip patch instrument(s) ({} PSG, {} FM)",
                  patch_instruments.len(), psg_n, fm_n,
        );
    }

    for entry in source.list("instruments", Some("instrument")) {
        let rel = if entry.starts_with("instruments/") || entry.starts_with("./instruments/") {
            entry.trim_start_matches("./").to_string()
        } else {
            format!("instruments/{}", entry)
        };
        let Some(text) = source.read_text(&rel)? else { continue };
        let Ok(inst) = crate::deps::instruments::parse(&text, &rel) else { continue };
        let stem = rel.rsplit('/').next()
        .and_then(|n| n.strip_suffix(".instrument")).unwrap_or("instrument");
        let sidecar_rel = format!("instruments/{}.instrument", stem);

        if inst.is_sf2() {
            let sf2_rel = inst.sf2_path.to_string_lossy().trim_start_matches("./").to_string();
            let Some(sf2_bytes) = source.read_bytes(&sf2_rel)? else { continue };
            let sf2_on_disk = match materialize_sf2_file(source, &sf2_rel) {
                Ok(Some(p)) => p,
                _ => continue,
            };

            if let Ok(obj) = build_sf2_instrument(
                &sf2_bytes, inst.sf2_bank as u16, inst.sf2_preset as u16,
                next_sf2_id, &mut samples,
                if inst.slicex {
                    Some(Sf2InstrumentOverrides {
                        pan: inst.slicex_pan as f64,
                        gain: inst.slicex_gain,
                        tune_cents: inst.slicex_tune_cents,
                        env: Sf2VoiceEnvelope {
                            peak: 1.0,
                            delay_sec: inst.slicex_delay_sec,
                            attack_sec: inst.slicex_attack_sec,
                            hold_sec: inst.slicex_hold_sec,
                            decay_sec: inst.slicex_decay_sec,
                            sustain_fraction: inst.slicex_sustain_fraction,
                            release_sec: inst.slicex_release_sec,
                        },
                    })
                } else { None },
            ) {
                next_sf2_id += obj.zone_count_hint() + 1;
                sf2_instruments.insert(sidecar_rel.clone(), Arc::new(obj));
            }

            sf2_engine_specs.insert(
                sidecar_rel,
                Sf2EngineSpec {
                    sf2_path: sf2_on_disk,
                    bank: inst.sf2_bank as u8, preset: inst.sf2_preset as u8,
                    gain: inst.gain * if inst.slicex { inst.slicex_gain } else { 1.0 },
                    iid: inst.source_channel,
                },
            );
        } else if inst.is_vst3() {
            if inst.unresolved { continue; }

            let plugin_path = embedded_plugins.get(&inst.plugin).cloned().filter(|p| p.exists())
            .or_else(|| {
                let r = PathBuf::from(&inst.vst_path);
                if r.exists() { Some(r) } else { None }
            })
            .or_else(|| {
                let idx = vst_index.get_or_insert_with(PluginIndex::scan);
                idx.find_vst3(&inst.plugin).cloned()
            });

            let Some(plugin_path) = plugin_path else { continue };

            let mut state_bytes = if inst.state_path.as_os_str().is_empty() {
                Vec::new()
            } else {
                let name = inst.state_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                let state_rel = format!("instruments/{}", name);
                source.read_bytes(&state_rel)?.unwrap_or_default()
            };

            if inst.plugin == "SFLT" && !state_bytes.is_empty() {
                let _ = materialize_sf2_reference(source, &mut state_bytes);
            }

            vst_aliases.push((sidecar_rel.clone(), inst.plugin.clone()));

            vst_instruments.insert(
                sidecar_rel,
                Arc::new(VstInstrument {
                    iid: inst.source_channel,
                    name: inst.plugin.clone(),
                         plugin_path, state_bytes,
                         fallback: inst.fallback,
                         gain: inst.gain * VST_TRIM * if inst.fallback { FALLBACK_VST_TRIM } else { 1.0 },
                }),
            );
        }
    }

    // Alias by sidecar stem (intent name) and plugin name so migrated
    // references resolve. First sidecar with a given key wins. Every alias
    // shares the same Arc as the sidecar entry.
    for (sidecar, plugin) in vst_aliases {
        let Some(arc) = vst_instruments.get(&sidecar).cloned() else { continue };
        if let Some(stem) = Path::new(&sidecar).file_stem().and_then(|s| s.to_str()) {
            vst_instruments
            .entry(format!("{}.vst3", stem))
            .or_insert_with(|| arc.clone());
        }
        if !plugin.is_empty() {
            vst_instruments
            .entry(format!("{}.vst3", plugin))
            .or_insert_with(|| arc.clone());
        }
    }

    // ── MIGRATION: VST references without .instrument sidecars ──────────
    // Parts may say `ƒ'ZENOLOGY.vst3'` even when `ZENOLOGY.instrument`
    // doesn't exist. Resolve each unresolved VST reference by:
    //   1. Trying the direct name (`ZENOLOGY` → plugin scan)
    //   2. Falling back to `parsed.fallbacks[key]` (substitute plugin + gain)
    // Registers under the halotext's reference key so parts resolve.

    let mut vst_keys_needed = HashSet::new();
    for part in parsed.parts.values() {
        let p = &part.sample_path;
        if p.ends_with(".vst3") || p.ends_with(".vst") || p.ends_with(".so") {
            vst_keys_needed.insert(p.clone());
        }
    }

    for key in vst_keys_needed {
        if vst_instruments.contains_key(&key) { continue; }

        let direct_name = Path::new(&key)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

        let direct = embedded_plugins.get(&direct_name).cloned().filter(|p| p.exists())
        .or_else(|| {
            let idx = vst_index.get_or_insert_with(PluginIndex::scan);
            idx.find_vst3(&direct_name).cloned()
        });

        let (plugin_name, plugin_path, gain_mult, is_fallback) = if let Some(path) = direct {
            (direct_name.clone(), path, 1.0f32, false)
        } else if let Some(spec) = parsed.fallbacks.get(&key) {
            let use_name = Path::new(&spec.use_)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&spec.use_)
            .to_string();

            let path = embedded_plugins.get(&use_name).cloned().filter(|p| p.exists())
            .or_else(|| {
                let idx = vst_index.get_or_insert_with(PluginIndex::scan);
                idx.find_vst3(&use_name).cloned()
            });

            let Some(path) = path else {
                crate::log_event!("player.vst.unresolved_fallback", key: key.clone(), plugin: use_name.clone());
                continue;
            };

            let g = spec.gain.unwrap_or(1.0) as f32;
            (use_name, path, g, true)
        } else {
            crate::log_event!("player.vst.unresolved", key: key.clone());
            continue;
        };

        // Keep the original iid from fidelity.json so scope mapping stays
        // intact. If there is no match, pick the lowest id that no VST *or
        // SF2 engine* already uses (SF2 channels share the 0..15 range).
        let source_iid = fidelity.as_ref().and_then(|f| {
            f.channels.values().find(|c| {
                c.name == direct_name || c.name == plugin_name
            }).map(|c| c.iid as u16)
        }).unwrap_or_else(|| {
            let mut id = 0;
            while vst_instruments.values().any(|v| v.iid == id)
                || sf2_engine_specs.values().any(|s| s.iid == id)
                {
                    id += 1;
                }
                id
        });

        let inst = Arc::new(VstInstrument {
            iid: source_iid,
            name: plugin_name.clone(),
                            plugin_path,
                            state_bytes: Vec::new(),
                            fallback: is_fallback,
                            gain: gain_mult * VST_TRIM * if is_fallback { FALLBACK_VST_TRIM } else { 1.0 },
        });

        vst_instruments.insert(key.clone(), Arc::clone(&inst));
        if plugin_name != direct_name {
            vst_instruments.entry(format!("{}.vst3", plugin_name)).or_insert_with(|| inst.clone());
        }
        vst_instruments.insert(direct_name.clone(), inst);
    }

    // ── MIGRATION: SF2 references without .instrument sidecars ─────────
    // Parts may say `ƒ'samples/X.sf2'(preset)` directly.  Resolve each
    // unique (path, bank, preset) triple into an Sf2Instrument + Sf2EngineSpec,
    // keyed by the composite `path#preset` string.  Parts referencing the
    // old `.instrument` sidecar form are already registered above and are
    // left alone.

    let mut sf2_keys_needed: HashSet<(String, i16, i16)> = HashSet::new();
    for part in parsed.parts.values() {
        if part.sample_path.ends_with(".sf2") {
            let preset = part.preset.unwrap_or(0);
            let bank = part.bank.unwrap_or(0);
            sf2_keys_needed.insert((part.sample_path.clone(), bank, preset));
        }
    }

    for (path, bank, preset) in sf2_keys_needed {
        let composite = if bank == 0 {
            format!("{}#{}", path, preset)
        } else {
            format!("{}#{}:{}", path, bank, preset)
        };

        if sf2_engine_specs.contains_key(&composite) {
            continue;
        }

        let sf2_bytes = match source.read_bytes(&path) {
            Ok(Some(b)) => b,
            _ => {
                crate::log_event!("player.sf2.read_failed", path: path.clone());
                continue;
            }
        };
        let sf2_on_disk = match materialize_sf2_file(source, &path) {
            Ok(Some(p)) => p,
            _ => continue,
        };

        // Allocate iid avoiding VSTs and existing SF2 engines
        let iid = {
            let mut id = 0u16;
            while id < u16::MAX
                && (vst_instruments.values().any(|v| v.iid == id)
                || sf2_engine_specs.values().any(|s| s.iid == id))
                {
                    id += 1;
                }
                id
        };

        if let Ok(obj) = build_sf2_instrument(
            &sf2_bytes,
            bank as u16,
            preset as u16,
            next_sf2_id,
            &mut samples,
            None,
        ) {
            next_sf2_id += obj.zone_count_hint() + 1;
            sf2_instruments.insert(composite.clone(), Arc::new(obj));
        }

        sf2_engine_specs.insert(
            composite.clone(),
                                Sf2EngineSpec {
                                    sf2_path: sf2_on_disk,
                                    bank: bank as u8,
                                    preset: preset as u8,
                                    gain: 1.0,
                                    iid,
                                },
        );

        // Alias basename form so `ƒ'X.sf2'(preset)` also resolves
        if let Some(base) = Path::new(&path).file_name().and_then(|s| s.to_str()) {
            let base_composite = if bank == 0 {
                format!("{}#{}", base, preset)
            } else {
                format!("{}#{}:{}", base, bank, preset)
            };
            if base_composite != composite {
                if let Some(arc) = sf2_instruments.get(&composite).cloned() {
                    sf2_instruments.entry(base_composite.clone()).or_insert(arc);
                }
                if let Some(spec) = sf2_engine_specs.get(&composite).cloned() {
                    sf2_engine_specs.entry(base_composite).or_insert(spec);
                }
            }
        }
    }
    // ─────────────────────────────────────────────────────────────────────

    // Fallback gain spread, applied once per *unique* instrument. Aliases
    // share an Arc, so counting map entries overcounts and over-attenuates,
    // and replacing each entry separately would split one instrument into
    // several unrelated Arcs. Scale each unique instrument once and point
    // every alias at the same scaled Arc.
    {
        let mut unique: HashMap<*const VstInstrument, Arc<VstInstrument>> = HashMap::new();
        for v in vst_instruments.values() {
            if v.fallback {
                unique.entry(Arc::as_ptr(v)).or_insert_with(|| Arc::clone(v));
            }
        }
        let n_fallback = unique.len();
        if n_fallback > 1 {
            let spread = 1.0 / (n_fallback as f32).sqrt();
            let scaled: HashMap<*const VstInstrument, Arc<VstInstrument>> = unique
            .iter()
            .map(|(ptr, v)| {
                (*ptr, Arc::new(VstInstrument {
                    iid: v.iid,
                    name: v.name.clone(),
                                plugin_path: v.plugin_path.clone(),
                                state_bytes: v.state_bytes.clone(),
                                fallback: v.fallback,
                                gain: v.gain * spread,
                }))
            })
            .collect();
            for v in vst_instruments.values_mut() {
                if let Some(s) = scaled.get(&Arc::as_ptr(v)) {
                    *v = Arc::clone(s);
                }
            }
        }
    }
    // ───────────────────────────────────────────────────────────────────

    let requires: Option<Requires> = match source.read_text(requires::FILENAME)? {
        Some(text) => requires::parse(&text).ok(),
        None => None,
    };

    let (chains, channel_to_insert) = inserts::load_chain_metadata(source);

    let sr = sample_rate as f64;
    let rows_per_measure = cfg.time_signature.0 as f64 * cfg.subdivisions.max(1) as f64;

    let mut resolved_lines = Vec::with_capacity(parsed.block_list.len());
    let mut total_arrangement_rows: f64 = 0.0;
    for line in &parsed.block_list {
        let mut rl = resolve_line(line, &parsed.blocks, &parsed.parts, rows_per_measure);

        for call in &rl.calls {
            if let Some(part) = parsed.parts.get(&call.name) {
                if is_chip_playable_part(&part.sample_path) {
                    let measure_end = call.pattern_measure_start + call.pattern_measure_len;
                    for ev in &part.events {
                        if ev.is_waypoint() { continue; }
                        if ev.row < call.pattern_measure_start || ev.row >= measure_end { continue; }
                        if let Some(dur) = ev.dur_beats {
                            let dur_rows = dur * part.subs_per_beat.max(1) as f64;
                            let end_row = call.line_offset
                            + (ev.row - call.pattern_measure_start)
                            + dur_rows;
                            if end_row > rl.duration + 0.1 {
                                rl.duration = end_row;
                            }
                        }
                    }
                }
            }
        }

        total_arrangement_rows += rl.duration;
        resolved_lines.push(rl);
    }

    if let Some(secs) = vgm_bytes_opt.as_deref().and_then(vgm_seconds) {
        let rows = secs * (cfg.bpm as f64 / 60.0) * cfg.subdivisions.max(1) as f64;
        if rows > total_arrangement_rows {
            total_arrangement_rows = rows;
        }
    }

    let mut tempo_events = parsed.tempo_events.clone();
    let shim_tempo = tempo_shims_to_events(&parsed.shims, rows_per_measure);
    if !shim_tempo.is_empty() {
        tempo_events.extend(shim_tempo);
        tempo_events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    }

    let tempo = TempoMap::new(cfg.bpm, &tempo_events, cfg.subdivisions);
    tempo.prepare(total_arrangement_rows);

    let lyrics = parsed.lyrics.into_iter().map(|line| LyricLine {
        start_sec: tempo.time_at_row(line.start_row),
                                               end_sec: tempo.time_at_row(line.end_row),
                                               segments: line.segments.into_iter().map(|s| LyricSegment {
                                                   time_sec: tempo.time_at_row(s.row),
                                                                                       text: s.text,
                                               }).collect(),
    }).collect();

    let render_end_sec = tempo.time_at_row(total_arrangement_rows) + TAIL_SECONDS;
    let total_samples = (render_end_sec * sr) as i64;

    let part_hash: HashMap<String, u64> = parsed.parts.iter()
    .map(|(n, p)| (n.clone(), hash_part(p, &path_to_id, &fps)))
    .collect();

    let mut cache = Cache::new(use_cache);
    let mut lines: Vec<LineInfo> = Vec::with_capacity(resolved_lines.len());
    let mut total_voices = 0usize;
    let mut cursor_row: f64 = 0.0;

    for rl in resolved_lines {
        let mut max_end_row = f64::NEG_INFINITY;
        for call in &rl.calls {
            let Some(part) = parsed.parts.get(&call.name) else { continue };

            if is_chip_playable_part(&part.sample_path) {
                max_end_row = max_end_row.max(rl.duration);
                let measure_end = call.pattern_measure_start + call.pattern_measure_len;
                for ev in &part.events {
                    if ev.is_waypoint() { continue; }
                    if ev.row >= call.pattern_measure_start && ev.row < measure_end {
                        total_voices += 1;
                    }
                }
                continue;
            }

            let tpl = cache.template(
                part_hash[&call.name], part, &path_to_id, &samples,
                &sf2_instruments, &sf2_engine_specs, &vst_instruments,
                &multi_sample_instruments,
            );
            let measure_end = call.pattern_measure_start + call.pattern_measure_len;
            for tv in &tpl.voices {
                if tv.rel_row < call.pattern_measure_start || tv.rel_row >= measure_end { continue; }
                let local = tv.rel_row - call.pattern_measure_start;
                let line_row = call.line_offset + local;
                max_end_row = max_end_row.max(line_row + tv.dur_rows);
                total_voices += 1;
            }
            for vn in &tpl.vst_notes {
                if vn.rel_row < call.pattern_measure_start || vn.rel_row >= measure_end { continue; }
                let local = vn.rel_row - call.pattern_measure_start;
                let line_row = call.line_offset + local;
                max_end_row = max_end_row.max(line_row + vn.dur_rows);
                total_voices += 1;
            }
        }

        let this_start_row = cursor_row;
        let this_end_row = cursor_row + max_end_row.max(rl.duration);
        let (min_start, max_end) = (
            (tempo.time_at_row(this_start_row) * sr) as i64,
                                    (tempo.time_at_row(this_end_row.min(total_arrangement_rows)) * sr) as i64,
        );

        lines.push(LineInfo { parts: rl.calls, start_row: cursor_row, min_start, max_end });
        cursor_row += rl.duration;
    }

    if total_voices == 0 && (!vgm_instruments.is_empty() || !patch_instruments.is_empty()) {
        total_voices = 1;
    }

    if total_voices == 0 {
        bail!("no playable voices (check samples.json and sample paths)");
    }

    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by_key(|&i| (lines[i].min_start, i));

    let title = title_hint.map(|s| s.to_string()).unwrap_or_else(|| "untitled".to_string());

    let project = Project {
        parts: parsed.parts, part_hash, samples, path_to_id,
        sf2_instruments, sf2_engine_specs, vst_instruments, vgm_instruments,
        vgm_dac,
        multi_sample_instruments,
        patch_instruments, patch_iids,
        lines, order, tempo, rows_per_measure,
        render_end_sec, total_samples, song_volume: cfg.song_volume / 100.0,
        linear_pan: cfg.panning_law == "linear", sample_rate, title,
        stereo_width: cfg.stereo_width, // <--- NEW
        initial_bpm: cfg.bpm, time_signature: cfg.time_signature,
        voice_count: total_voices, source_dir, cover_path, fidelity, requires,
        shims: parsed.shims, lyrics, chains, channel_to_insert,
        note_rows_cache: std::sync::OnceLock::new(),
    };
    Ok((Arc::new(project), cache))
}

pub(crate) fn load_project_from_dir(
    ts_path: &Path, sample_rate: u32, use_cache: bool, quiet: bool,
) -> Result<(Arc<Project>, Cache)> {
    let base = ts_path.parent().unwrap_or_else(|| Path::new("."));
    let name = ts_path.file_name().and_then(|s| s.to_str()).unwrap_or("halotext");
    let title = ts_path.file_stem().and_then(|s| s.to_str());
    let source = DirSource::new(base.to_path_buf(), name.to_string());
    load_project_from_source(&source, sample_rate, use_cache, quiet, title)
}

pub(crate) fn load_project_from_archive(
    archive_path: &Path, sample_rate: u32, use_cache: bool, quiet: bool,
) -> Result<(Arc<Project>, Cache)> {
    let reader = crate::archive::ArchiveReader::open(archive_path)?;
    let title = reader.manifest().title.clone();
    load_project_from_source(&reader, sample_rate, use_cache, quiet, Some(&title))
}

pub(crate) fn load_project(
    path: &Path, sample_rate: u32, use_cache: bool, quiet: bool,
) -> Result<(Arc<Project>, Cache)> {
    if crate::archive::is_archive_path(path) {
        load_project_from_archive(path, sample_rate, use_cache, quiet)
    } else {
        load_project_from_dir(path, sample_rate, use_cache, quiet)
    }
}
