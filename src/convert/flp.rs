// src/convert/flp.rs
//! FLP → Trackscript converter.
//!
//! The work is split across submodules:
//!   * `classify`  — which channels are usable, and how
//!   * `resolve`   — installed / fallback / unresolved plugin resolution
//!   * `extract`   — fidelity.json + insert chain extraction
//!   * `place`     — playlist → (channel, tick) placements
//!   * `render`    — notes in a measure → Trackscript part
//!   * `sf2`       — SoundFont state parsing
//!   * `wav`       — sample decoding and lookup
//!
//! This file owns the flow: parse → classify → render → assemble.

mod classify;
mod extract;
mod place;
mod render;
mod resolve;
mod sf2;
mod wav;

use anyhow::{bail, Result};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use super::{
    fmt, safe_name, AuxFile, ConvertOptions, Converted, SampleJsonEntry, SampleOut,
    SamplesJson,
};
use crate::fallbacks::FallbackTable;
use crate::fidelity::MultiSampleFidelity;
use crate::plugins::PluginIndex;
use crate::sources::flp as flp_src;

use classify::{classify_channels, Classified};
use extract::{build_fidelity, extract_inserts};
use place::{end_tick_of, notes_in_measure, place_notes};
use render::{emit_part, NoteStyle, Part};
use resolve::Resolved;
use wav::{decode_audio_to_wav, wav_sample_rate, wav_secs};

// ── constants shared with submodules ───────────────────────────────────

const DEFAULT_FLPPQ: u32 = 96;
const DEFAULT_TEMPO: f64 = 120.0;
pub(super) const PLAY_FULL_SAMPLE: bool = true;
pub(super) const MAX_FULL_BEATS: f64 = 4.0;
pub(super) const MAX_NOTE_TICKS: u32 = 500_000;
pub(super) const MIN_EMBEDDED_BYTES: usize = 512;
pub(super) const FL_C5_MIDI: i32 = 60;
pub(super) const FL_C5_KEY: u16 = 60;

/// Built-in generators with no vst_path and a state smaller than this are
/// checked for a SoundFont path.  Anything bigger (DirectWave, Serum, ...)
/// can't be the SoundFont player.
pub(super) const SF2_STATE_MAX: usize = 4096;

// ── shared helpers ─────────────────────────────────────────────────────

pub(super) fn debug_on() -> bool {
    std::env::var_os("FLP_DEBUG").is_some()
}

pub(super) fn hex(b: &[u8]) -> String {
    b.iter()
    .map(|x| format!("{:02x}", x))
    .collect::<Vec<_>>()
    .join(" ")
}

// ── main entry ─────────────────────────────────────────────────────────

pub(super) fn convert(
    prj: &flp_src::Project,
    src_path: &Path,
    opts: &ConvertOptions,
) -> Result<Converted> {
    let subs = opts.subdivisions;
    let ppq = if prj.ppq == 0 {
        DEFAULT_FLPPQ
    } else {
        prj.ppq as u32
    };
    let ticks_per_measure = ppq * 4;
    let flp_dir = src_path.parent().unwrap_or_else(|| Path::new("."));
    let tempo = if prj.tempo > 0.0 {
        prj.tempo
    } else {
        DEFAULT_TEMPO
    };

    let (placed, used_playlist, dropped_notes) = place_notes(prj);

    let plugin_index = PluginIndex::scan();
    let fallback_table = match FallbackTable::load_default() {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("flp: fallback table unavailable ({e:#}); native channels use exact match only");
            None
        }
    };

    if debug_on() {
        eprintln!(
            "flp: ppq {} tempo {} playlist items {}",
            ppq,
            tempo,
            prj.playlist.len()
        );
        for c in &prj.channels {
            eprintln!(
                "flp: chan {:>3} {:<10} placed {:>5} env {:>2} levels={} sample {:?} name {:?}",
                c.iid,
                c.channel_type,
                placed.get(&c.iid).map_or(0, |v| v.len()),
                      c.envelopes.len(),
                      c.levels.is_some(),
                      c.sample_path,
                      c.name
            );
        }
    }

    let mut aux_files: Vec<AuxFile> = Vec::new();
    let mut multi_zones: BTreeMap<String, MultiSampleFidelity> = BTreeMap::new();

    let mut warnings: Vec<String> = Vec::new();
    if dropped_notes > 0 {
        warnings.push(format!(
            "{} note(s) dropped as misdecoded (position > {} ticks)",
                              dropped_notes, MAX_NOTE_TICKS
        ));
    }

    // ── classify channels ────────────────────────────────────────────
    let Classified {
        usable_samples,
        usable_embedded,
        usable_natives,
        usable_sf2,
        resolved_map,
        sidecar_path,
        skipped,
        warnings: cls_warnings,
        envelope_only,
    } = classify_channels(
        prj,
        &placed,
        opts,
        flp_dir,
        &plugin_index,
        fallback_table.as_ref(),
        &mut aux_files,
    );
    warnings.extend(cls_warnings);

    if usable_samples.is_empty()
        && usable_embedded.is_empty()
        && usable_natives.is_empty()
        && usable_sf2.is_empty()
    {
        let detail = skipped
        .iter()
        .map(|(c, w)| {
            let name = if c.name.is_empty() {
                &c.channel_type
            } else {
                &c.name
            };
            format!("  [{:2}] {}: {}", c.iid, name, w)
        })
        .collect::<Vec<_>>()
        .join("\n");
        bail!("Nothing convertible.\n{}", detail);
    }

    // ── end tick / measure count ─────────────────────────────────────
    let mut end_tick: u32 = 0;
    for (chan, _) in &usable_samples {
        end_tick = end_tick.max(end_tick_of(&placed, chan.iid));
    }
    for (chan, _) in &usable_embedded {
        end_tick = end_tick.max(end_tick_of(&placed, chan.iid));
    }
    for chan in &usable_natives {
        end_tick = end_tick.max(end_tick_of(&placed, chan.iid));
    }
    for chan in &usable_sf2 {
        end_tick = end_tick.max(end_tick_of(&placed, chan.iid));
    }
    let n_measures = std::cmp::max(1, (end_tick + ticks_per_measure - 1) / ticks_per_measure);

    // ── sample files ─────────────────────────────────────────────────
    let mut sample_files: HashMap<u16, String> = HashMap::new();
    let mut sample_meta: Vec<SampleJsonEntry> = Vec::new();
    let mut sample_outs: Vec<SampleOut> = Vec::new();
    let mut styles: HashMap<u16, NoteStyle> = HashMap::new();

    let mut decoded: Vec<(&flp_src::Channel, PathBuf)> =
        Vec::with_capacity(usable_samples.len());

    for (chan, src) in &usable_samples {
        let raw_name = if chan.name.is_empty() {
            src.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("sample")
            .to_string()
        } else {
            chan.name.clone()
        };
        let target_name = format!("{:02}_{}.wav", chan.iid, safe_name(&raw_name));
        let rel = format!("samples/{}", target_name);

        let bytes = match decode_audio_to_wav(src) {
            Ok(b) => b,
            Err(e) => {
                warnings.push(format!(
                    "[{:2}] {}: failed to decode {}: {:#}",
                    chan.iid,
                    if chan.name.is_empty() {
                        "<unnamed>"
                    } else {
                        &chan.name
                    },
                    src.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
                                      e,
                ));
                continue;
            }
        };

        let sample_rate = wav_sample_rate(&bytes).unwrap_or(44100);
        styles.insert(
            chan.iid,
            NoteStyle {
                transpose: 0,
                full_secs: wav_secs(&bytes),
                      bpm: tempo,
                      vol_scale: chan.volume_scale(),
                      pan_offset: chan.pan_norm(),
                      sample_rate,
            },
        );
        sample_outs.push(SampleOut {
            rel_path: rel.clone(),
                         wav: bytes,
        });
        sample_files.insert(chan.iid, rel.clone());
        sample_meta.push(SampleJsonEntry {
            id: chan.iid,
            name: if chan.name.is_empty() {
                raw_name
            } else {
                chan.name.clone()
            },
            file: rel,
            c5_speed: sample_rate,
        });
        decoded.push((*chan, src.clone()));
    }

    let usable_samples = decoded;

    // ── embedded samples ─────────────────────────────────────────────
    // Each zone gets its own entry in samples.json with a distinct id.
    // Zone 0 keeps the channel's iid; zones 1+ use
    // 20000 + iid * 10 + zi, which cannot collide with any iid or with
    // the SF2 id range (10000..10100).
    for (chan, emb) in &usable_embedded {
        let raw_name = if chan.name.is_empty() {
            format!("Ch{:02}", chan.iid)
        } else {
            chan.name.clone()
        };
        let safe = safe_name(&raw_name);
        let primary_rel = format!("samples/{:02}_{}.wav", chan.iid, safe);

        let sample_rate = wav_sample_rate(&emb.wav).unwrap_or(44100);
        styles.insert(
            chan.iid,
            NoteStyle {
                transpose: emb.root_key.map_or(0, |r| FL_C5_MIDI - r as i32),
                      full_secs: wav_secs(&emb.wav),
                      bpm: tempo,
                      vol_scale: chan.volume_scale(),
                      pan_offset: chan.pan_norm(),
                      sample_rate,
            },
        );
        sample_files.insert(chan.iid, primary_rel.clone());

        let mut zone_files: Vec<String> = Vec::new();
        let mut zone_roots: Vec<Option<i32>> = Vec::new();
        for (zi, (wav, root)) in emb.all_zones.iter().enumerate() {
            let rel = if zi == 0 {
                primary_rel.clone()
            } else {
                format!("samples/{:02}_{}_zone{:02}.wav", chan.iid, safe, zi)
            };
            let zone_id: u16 = if zi == 0 {
                chan.iid
            } else {
                (20000u32 + (chan.iid as u32) * 10 + zi as u32) as u16
            };
            let zone_name = if zi == 0 {
                raw_name.clone()
            } else {
                format!("{}_zone{:02}", raw_name, zi)
            };
            let zone_rate = wav_sample_rate(wav).unwrap_or(sample_rate);

            sample_outs.push(SampleOut {
                rel_path: rel.clone(),
                             wav: wav.clone(),
            });
            sample_meta.push(SampleJsonEntry {
                id: zone_id,
                name: zone_name,
                file: rel.clone(),
                c5_speed: zone_rate,
            });

            zone_files.push(rel);
            zone_roots.push(root.map(|r| r as i32));
        }
        if !zone_files.is_empty() {
            multi_zones.insert(
                chan.iid.to_string(),
                MultiSampleFidelity {
                    files: zone_files,
                    root_keys: zone_roots,
                },
            );
        }
    }

    // ── native + SF2 styles ──────────────────────────────────────────
    // Fallback plugins get a per-fallback gain trim so their default
    // patches don't dominate the mix.  Baking it into vol_scale means
    // the emitted `%` values already account for the difference and
    // the player needs no changes.
    for chan in usable_natives.iter().chain(usable_sf2.iter()) {
        let gain = match resolved_map.get(&chan.iid) {
            Some(Resolved::Fallback { gain, .. }) => *gain,
            _ => 1.0,
        };
        styles.insert(
            chan.iid,
            NoteStyle {
                vol_scale: chan.volume_scale() * gain as f64,
                      pan_offset: chan.pan_norm(),
                      ..NoteStyle::neutral(tempo)
            },
        );
    }

    // ── render parts ─────────────────────────────────────────────────
    let mut seen: HashMap<(u16, String), usize> = HashMap::new();
    let mut parts: Vec<Part> = Vec::new();
    let mut part_name_used: HashMap<String, u32> = HashMap::new();
    let mut placements: Vec<Vec<usize>> = Vec::new();

    for m in 0..n_measures {
        let m_start = m * ticks_per_measure;
        let m_end = m_start + ticks_per_measure;
        let mut row: Vec<usize> = Vec::new();

        let mut channels_here: Vec<(&flp_src::Channel, &'static str)> = Vec::new();
        channels_here.extend(usable_samples.iter().map(|(c, _)| (*c, "sample")));
        channels_here.extend(usable_embedded.iter().map(|(c, _)| (*c, "sample")));
        channels_here.extend(usable_natives.iter().map(|c| (*c, "instrument")));
        channels_here.extend(usable_sf2.iter().map(|c| (*c, "instrument")));

        for (chan, kind) in channels_here {
            let notes_here = notes_in_measure(&placed, chan.iid, m_start, m_end);
            if notes_here.is_empty() {
                continue;
            }
            let source_path = match kind {
                "sample" => sample_files
                .get(&chan.iid)
                .map(|s| s.as_str())
                .unwrap_or(""),
                "instrument" => sidecar_path
                .get(&chan.iid)
                .map(|s| s.as_str())
                .unwrap_or(""),
                _ => "",
            };
            let idx = emit_part(
                &mut seen,
                &mut parts,
                &mut part_name_used,
                chan,
                &notes_here,
                m_start,
                subs,
                ppq,
                kind,
                styles
                .get(&chan.iid)
                .copied()
                .unwrap_or(NoteStyle::neutral(tempo)),
                                source_path,
            );
            row.push(idx);
        }

        placements.push(row);
    }

    // ── assemble trackscript ─────────────────────────────────────────
    let mut out = String::new();
    out.push_str("Config\n{\n");
    out.push_str("    TrackscriptVersion: 1.10;\n");
    out.push_str(&format!("    BeatsPerMinute: {};\n", fmt(tempo, 2)));
    out.push_str("    TimeSignature: 4/4;\n");
    out.push_str(&format!("    Subdivisions: {};\n", subs));
    out.push_str("    SongVolume: 100%;\n");
    out.push_str("    PanningLaw: linear;\n");
    out.push_str("    Overlap: stack;\n");
    out.push_str("    PitchDefault: names;\n");
    out.push_str("}\n\n");

    out.push_str("BlockList()\n{\n");
    for row in &placements {
        if row.is_empty() {
            out.push_str("    ;\n");
        } else {
            out.push_str("    ");
            for &idx in row {
                out.push_str(&crate::spec::call(&parts[idx].name, ""));
                out.push(' ');
            }
            out.push('\n');
        }
    }
    out.push_str("}\n\n");

    out.push_str("BlockArray()\n{\n");
    for part in &parts {
        let args = match part.kind {
            "sample" => crate::spec::source_arg(&sample_files[&part.iid], None),
            "instrument" => crate::spec::source_arg(&sidecar_path[&part.iid], None),
            _ => unreachable!(),
        };
        out.push_str("    ");
        out.push_str(&crate::spec::part_definition(
            &part.name,
            &args,
            4,
            subs,
            &part.rendered,
        ));
        out.push('\n');
    }
    out.push_str("}\n");

    out.push('\n');
    out.push_str("// ------------------------------------------------------------\n");
    let title = if prj.title.is_empty() {
        src_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("(untitled)")
    } else {
        &prj.title
    };
    out.push_str(&format!("// {}\n", title));
    out.push_str("// converted from FL Studio project by flp2trackscript (rust)\n");
    if !prj.author.is_empty() {
        out.push_str(&format!("// author: {}\n", prj.author));
    }
    if !prj.genre.is_empty() {
        out.push_str(&format!("// genre: {}\n", prj.genre));
    }
    if prj.tempo <= 0.0 {
        out.push_str(&format!(
            "//\n// No tempo found in project; using {} BPM.\n",
            DEFAULT_TEMPO
        ));
    }
    if !used_playlist {
        out.push_str(
            "//\n// No pattern clips found in the playlist: patterns were laid out at their\n\
// own local positions (all patterns overlap from the start).\n",
        );
    }

    if !envelope_only.is_empty() {
        out.push_str(
            "//\n// Channels with active sampler envelopes (parsed, not yet rendered):\n",
        );
        for (iid, name, n) in &envelope_only {
            out.push_str(&format!(
                "//   [{:2}] {}: {} active envelope(s) — attack/decay/sustain/release not applied\n",
                                  iid, name, n
            ));
        }
    }

    if !usable_embedded.is_empty() {
        out.push_str("//\n// Embedded audio extracted from plugin state:\n");
        for (chan, emb) in &usable_embedded {
            let name = if chan.name.is_empty() {
                &chan.channel_type
            } else {
                &chan.name
            };
            out.push_str(&format!(
                "//   [{:2}] {}: {} file(s) found, {} zone(s) written to samples.json{}\n",
                                  chan.iid,
                                  name,
                                  emb.files_found,
                                  emb.all_zones.len(),
                                  match emb.root_key {
                                      Some(r) => format!(", root key MIDI {}", r),
                                  None => String::new(),
                                  },
            ));
        }
    }
    if !warnings.is_empty() {
        out.push_str("//\n// Warnings:\n");
        for w in &warnings {
            out.push_str(&format!("//   {}\n", w));
        }
    }
    if !skipped.is_empty() {
        out.push_str("//\n// Skipped:\n");
        for (c, w) in &skipped {
            let name = if c.name.is_empty() {
                &c.channel_type
            } else {
                &c.name
            };
            out.push_str(&format!("//   [{:2}] {}: {}\n", c.iid, name, w));
        }
    }

    let samples_json = serde_json::to_string_pretty(&SamplesJson {
        format: "trackscript-sample-library",
            version: 2,
            samples: sample_meta,
            instruments: vec![],
    })?;

    // ── fidelity sidecar ─────────────────────────────────────────────
    let mut fidelity = build_fidelity(prj);
    fidelity.multi_sample = multi_zones;
    fidelity.inserts = extract_inserts(
        prj,
        &plugin_index,
        fallback_table.as_ref(),
        &mut aux_files,
    );

    match fidelity.to_json() {
        Ok(json) => {
            aux_files.push(AuxFile {
                rel_path: crate::fidelity::FIDELITY_FILENAME.to_string(),
                bytes: json.into_bytes(),
            });
        }
        Err(e) => {
            warnings.push(format!("fidelity sidecar failed to serialize: {:#}", e));
        }
    }

    Ok(Converted {
        trackscript: out,
        samples: sample_outs,
        samples_json,
        aux: aux_files,
    })
}