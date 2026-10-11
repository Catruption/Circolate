// src/convert/flp.rs
//! FLP → Trackscript converter.
//!
//! Sampler channels become sample-sourced blocks.  Native channels:
//!   1. If the plugin state blob contains embedded audio (DirectWave and
//!      friends), extract and treat as a sampler.
//!   2. Else if a same-named `.sf2` sits next to the FLP, emit an
//!      `.instrument` sidecar pointing at it.  The player loads the SF2
//!      and picks the right sample per note (key + velocity ranges).
//!   3. Else emit a `vst3` sidecar with the raw state blob.
//!
//! Submodules:
//!   * `place`  — playlist + pattern → absolute-tick placements
//!   * `render` — placements → `@beats/subs[...]` blocks
//!   * `sf2`    — name matching for native → external `.sf2`
//!   * `wav`    — WAV decoding and sample-path lookup

mod place;
mod render;
mod sf2;
mod wav;

use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{
    fmt, identifier, safe_name, AuxFile, ConvertOptions, Converted, SampleJsonEntry, SampleOut,
    SamplesJson,
};
use crate::sources::flp::{self as flp_src, Channel};
use crate::sources::flp_audio;

use place::{end_tick_of, notes_in_measure, place_notes};
use render::{emit_part, EmbeddedSample, NoteStyle, Part};
use sf2::find_matching_sf2;
use wav::{decode_audio_to_wav, find_sample, wav_sample_rate, wav_secs};

const DEFAULT_FLPPQ: u32 = 96;
const DEFAULT_TEMPO: f64 = 120.0;
const PLAY_FULL_SAMPLE: bool = true;
const MAX_FULL_BEATS: f64 = 4.0;
const MAX_NOTE_TICKS: u32 = 500_000;
const MIN_EMBEDDED_BYTES: usize = 512;
const FL_C5_MIDI: i32 = 60;
const FL_C5_KEY: u16 = 60;

// ── helpers ────────────────────────────────────────────────────────────

pub(super) fn debug_on() -> bool {
    std::env::var_os("FLP_DEBUG").is_some()
}

pub(super) fn hex(b: &[u8]) -> String {
    b.iter()
    .map(|x| format!("{:02x}", x))
    .collect::<Vec<_>>()
    .join(" ")
}

// ── VST sidecar ────────────────────────────────────────────────────────

fn vst_sidecar(base_name: &str, chan: &Channel) -> Result<Vec<AuxFile>> {
    let plugin = chan.plugin.as_ref().expect("caller checked plugin presence");

    let state_rel = format!("instruments/{}.state", base_name);
    let inst_rel = format!("instruments/{}.instrument", base_name);

    let mut lines = vec![
        format!("Instrument {}", base_name),
            "{".to_string(),
            "    Kind: vst3;".to_string(),
    ];
    if !plugin.internal_name.is_empty() {
        lines.push(format!("    Plugin: '{}';", plugin.internal_name));
    }
    if !plugin.vst_path.is_empty() {
        let normalized = plugin.vst_path.replace('\\', "/");
        let vst_name = Path::new(&normalized)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?");
        lines.push(format!("    Path: '{}';", vst_name));
    }
    if !plugin.display_name.is_empty() {
        lines.push(format!("    DisplayName: '{}';", plugin.display_name));
    }
    lines.push(format!("    State: '{}.state';", base_name));
    lines.push(format!("    SourceChannel: {};", chan.iid));
    lines.push("}".to_string());

    let inst_text = lines.join("\n") + "\n";

    Ok(vec![
        AuxFile {
            rel_path: state_rel,
            bytes: plugin.state.clone(),
        },
       AuxFile {
           rel_path: inst_rel,
           bytes: inst_text.into_bytes(),
       },
    ])
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

    // Declared up here so the channel loop can populate them.
    let mut sidecar_path: HashMap<u16, String> = HashMap::new();
    let mut aux_files: Vec<AuxFile> = Vec::new();

    let mut usable_samples: Vec<(&Channel, PathBuf)> = Vec::new();
    let mut usable_embedded: Vec<(&Channel, EmbeddedSample)> = Vec::new();
    let mut usable_natives: Vec<&Channel> = Vec::new();
    // Channels matched to an external .sf2 (sidecar already emitted).
    let mut usable_sf2: Vec<&Channel> = Vec::new();
    let mut skipped: Vec<(&Channel, String)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if dropped_notes > 0 {
        warnings.push(format!(
            "{} note(s) dropped as misdecoded (position > {} ticks)",
                              dropped_notes, MAX_NOTE_TICKS
        ));
    }

    let mut envelope_only: Vec<(u16, String, usize)> = Vec::new();

    for chan in &prj.channels {
        if !chan.enabled.unwrap_or(true) {
            skipped.push((chan, "disabled in FL".into()));
            continue;
        }

        let active_envs = chan
        .envelopes
        .iter()
        .filter(|e| e.env_enabled != 0)
        .count();
        if active_envs > 0 {
            let label = if chan.name.is_empty() {
                chan.channel_type.clone()
            } else {
                chan.name.clone()
            };
            envelope_only.push((chan.iid, label, active_envs));
        }

        let has_notes = placed.get(&chan.iid).map_or(false, |v| !v.is_empty());
        if !has_notes {
            let has_content = !chan.sample_path.is_empty()
            || chan
            .plugin
            .as_ref()
            .map_or(false, |p| !p.state.is_empty());
            if has_content
                && matches!(
                    chan.channel_type.as_str(),
                            "sampler" | "instrument" | "native"
                )
                {
                    skipped.push((chan, "no notes".into()));
                }
                continue;
        }
        match chan.channel_type.as_str() {
            "sampler" | "instrument" => {
                if chan.sample_path.is_empty() {
                    skipped.push((chan, "no sample path".into()));
                    continue;
                }
                match find_sample(&chan.sample_path, opts.samples_dir.as_deref(), Some(flp_dir)) {
                    Some(p) => usable_samples.push((chan, p)),
                    None => {
                        let basename = Path::new(&chan.sample_path.replace('\\', "/"))
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("?")
                        .to_string();
                        skipped.push((chan, format!("sample not found: {}", basename)));
                    }
                }
            }
            "native" => {
                if opts.skip_native {
                    skipped.push((chan, "native channel; --skip-native".into()));
                    continue;
                }

                // 1. Embedded audio in plugin state?
                if let Some(plugin) = chan.plugin.as_ref() {
                    let found = flp_audio::extract_all(&plugin.state);
                    let files_found = found.len();
                    if debug_on() {
                        for (i, a) in found.iter().enumerate() {
                            eprintln!(
                                "flp: chan {} embedded #{} {} {} bytes, root {:?}",
                                chan.iid,
                                i,
                                a.extension,
                                a.data.len(),
                                      flp_audio::wav_root_key(&a.data)
                            );
                        }
                    }
                    let mut chosen: Option<EmbeddedSample> = None;
                    for aud in &found {
                        if aud.data.len() < MIN_EMBEDDED_BYTES {
                            continue;
                        }
                        let wav = if aud.extension == "flac" {
                            match flp_audio::flac_to_wav(&aud.data) {
                                Ok(w) => w,
                                Err(e) => {
                                    warnings.push(format!(
                                        "[{:2}] embedded FLAC failed to decode: {:#}",
                                        chan.iid, e
                                    ));
                                    continue;
                                }
                            }
                        } else {
                            aud.data.clone()
                        };
                        let root_key = flp_audio::wav_root_key(&wav);
                        chosen = Some(EmbeddedSample {
                            wav,
                            root_key,
                            files_found,
                            raw_len: aud.data.len(),
                        });
                        break;
                    }
                    if let Some(e) = chosen {
                        if e.files_found > 2 {
                            warnings.push(format!(
                                "[{:2}] {} audio files in plugin state; only the first is used \
(multi-sample instruments will sound wrong)",
                                                  chan.iid, e.files_found
                            ));
                        }
                        usable_embedded.push((chan, e));
                        continue;
                    }
                }

                // 2. External SF2 matching the channel name?
                let mut sf2_used = false;
                if !chan.name.is_empty() {
                    let dirs: [Option<&Path>; 2] =
                    [opts.samples_dir.as_deref(), Some(flp_dir)];
                    'sf2: for dir in dirs.into_iter().flatten() {
                        if let Some(sf2_src) = find_matching_sf2(&chan.name, dir) {
                            if debug_on() {
                                eprintln!(
                                    "flp: chan {} sf2 {} (sidecar)",
                                          chan.iid,
                                          sf2_src.display()
                                );
                            }
                            let base_raw = if chan.name.is_empty() {
                                format!("Ch{:02}", chan.iid)
                            } else {
                                chan.name.clone()
                            };
                            let base = identifier(&base_raw);
                            let sf2_name = format!("{:02}_sf2_{}.sf2", chan.iid, base);
                            let sf2_rel = format!("samples/{}", sf2_name);

                            let bytes = match std::fs::read(&sf2_src) {
                                Ok(b) => b,
                                Err(e) => {
                                    warnings.push(format!(
                                        "[{:2}] SF2 {} read failed: {:#}",
                                        chan.iid,
                                        sf2_src.display(),
                                                          e
                                    ));
                                    break 'sf2;
                                }
                            };
                            aux_files.push(AuxFile {
                                rel_path: sf2_rel.clone(),
                                           bytes,
                            });

                            let inst_rel =
                            format!("instruments/{}.instrument", base);
                            let inst_text = format!(
                                "Instrument {}\n{{\n    Kind: sf2;\n    File: '{}';\n    SourceChannel: {};\n}}\n",
                                base, sf2_rel, chan.iid
                            );
                            aux_files.push(AuxFile {
                                rel_path: inst_rel.clone(),
                                           bytes: inst_text.into_bytes(),
                            });

                            sidecar_path.insert(chan.iid, inst_rel);
                            sf2_used = true;
                            break 'sf2;
                        }
                    }
                }
                if sf2_used {
                    usable_sf2.push(chan);
                    continue;
                }

                // 3. VST sidecar.
                let has_state =
                chan.plugin.as_ref().map_or(false, |p| !p.state.is_empty());
                if !has_state {
                    skipped.push((chan, "native channel without saved state".into()));
                    continue;
                }
                usable_natives.push(chan);
            }
            other => {
                skipped.push((chan, format!("channel type '{}' not converted", other)));
            }
        }
    }

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
        let n_measures =
        std::cmp::max(1, (end_tick + ticks_per_measure - 1) / ticks_per_measure);

        let mut sample_files: HashMap<u16, String> = HashMap::new();
        let mut sample_meta: Vec<SampleJsonEntry> = Vec::new();
        let mut sample_outs: Vec<SampleOut> = Vec::new();
        let mut styles: HashMap<u16, NoteStyle> = HashMap::new();

        let mut decoded: Vec<(&Channel, PathBuf)> = Vec::with_capacity(usable_samples.len());

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
                c5_speed: 8363,
            });
            decoded.push((*chan, src.clone()));
        }

        let usable_samples = decoded;

        for (chan, emb) in &usable_embedded {
            let raw_name = if chan.name.is_empty() {
                format!("Ch{:02}", chan.iid)
            } else {
                chan.name.clone()
            };
            let target_name = format!("{:02}_{}.wav", chan.iid, safe_name(&raw_name));
            let rel = format!("samples/{}", target_name);

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
            sample_outs.push(SampleOut {
                rel_path: rel.clone(),
                             wav: emb.wav.clone(),
            });
            sample_files.insert(chan.iid, rel.clone());
            sample_meta.push(SampleJsonEntry {
                id: chan.iid,
                name: raw_name,
                file: rel,
                c5_speed: 8363,
            });
        }

        // VST sidecars (only for channels that fell through to usable_natives).
        {
            let mut used: HashMap<String, u32> = HashMap::new();
            for chan in &usable_natives {
                let base_raw = if chan.name.is_empty() {
                    format!("Ch{:02}", chan.iid)
                } else {
                    chan.name.clone()
                };
                let base = identifier(&base_raw);
                let count_ref = used.entry(base.clone()).or_insert(0);
                *count_ref += 1;
                let count = *count_ref;
                let sidecar_name = if count == 1 {
                    base
                } else {
                    format!("{}{:02}", base, count)
                };
                let files = vst_sidecar(&sidecar_name, chan)?;
                aux_files.extend(files);
                sidecar_path.insert(
                    chan.iid,
                    format!("instruments/{}.instrument", sidecar_name),
                );
            }
        }

        let mut seen: HashMap<(u16, String), usize> = HashMap::new();
        let mut parts: Vec<Part> = Vec::new();
        let mut part_name_used: HashMap<String, u32> = HashMap::new();
        let mut placements: Vec<Vec<usize>> = Vec::new();

        for m in 0..n_measures {
            let m_start = m * ticks_per_measure;
            let m_end = m_start + ticks_per_measure;
            let mut row: Vec<usize> = Vec::new();

            let mut channels_here: Vec<(&Channel, &'static str)> = Vec::new();
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
                    out.push_str(&format!("ƒ{};", parts[idx].name));
                }
                out.push('\n');
            }
        }
        out.push_str("}\n\n");

        out.push_str("BlockArray()\n{\n");
        for part in &parts {
            let source = match part.kind {
                "sample" => format!("ƒ'{}'()", sample_files[&part.iid]),
                "instrument" => format!("ƒ'{}'()", sidecar_path[&part.iid]),
                _ => unreachable!(),
            };
            out.push_str(&format!("    {} = {}:\n", part.name, source));
            out.push_str("    {\n");
            for line in part.rendered.lines() {
                out.push_str("        ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("    }\n\n");
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
                    "//   [{:2}] {}: {} file(s) found, using first ({} bytes){}\n",
                                      chan.iid,
                                      name,
                                      emb.files_found,
                                      emb.raw_len,
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

        Ok(Converted {
            trackscript: out,
            samples: sample_outs,
            samples_json,
            aux: aux_files,
            report: String::new(),
           semantics_json: None,
        })
}
