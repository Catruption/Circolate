
// src/convert/flp.rs
//! FLP → Trackscript converter.

mod classify;
mod extract;
mod place;
mod render;
mod resolve;
mod sf2;
mod slicex;
mod wav;

use anyhow::{bail, Result};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use super::song::{Call, MeasureLine, Song};
use super::{
    identifier, safe_name, AuxFile, ConvertOptions, Converted, SampleJsonEntry,
    SampleOut, SamplesJson,
};
use crate::deps::registry::RegistryTable;
use crate::fidelity::MultiSampleFidelity;
use crate::deps::index::PluginIndex;
use crate::formats::flp as flp_src;

use classify::{classify_channels, Classified};
use extract::{build_fidelity, extract_inserts};
use place::{end_tick_of, notes_in_measure, place_notes};
use render::{render_part, NoteStyle};
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

/// Built-in generators with no vst_path and a state smaller than this
/// are checked for a SoundFont path.
pub(super) const SF2_STATE_MAX: usize = 4096;

// ── shared helpers ─────────────────────────────────────────────────────

pub(super) fn debug_on() -> bool {
    std::env::var_os("FLP_DEBUG").is_some()
}

pub(super) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect::<Vec<_>>().join(" ")
}

// ── main entry ─────────────────────────────────────────────────────────

pub(super) fn convert(
    prj: &flp_src::Project,
    src_path: &Path,
    opts: &ConvertOptions,
) -> Result<Converted> {
    let subs = opts.subdivisions;
    let ppq = if prj.ppq == 0 { DEFAULT_FLPPQ } else { prj.ppq as u32 };
    let ticks_per_measure = ppq * 4;
    let flp_dir = src_path.parent().unwrap_or_else(|| Path::new("."));
    let tempo = if prj.tempo > 0.0 { prj.tempo } else { DEFAULT_TEMPO };

    let (placed, used_playlist, dropped_notes) = place_notes(prj);

    let plugin_index = PluginIndex::scan();
    let fallback_table = match RegistryTable::load() {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("flp: fallback table unavailable ({e:#}); native channels use exact match only");
            None
        }
    };

    if debug_on() {
        eprintln!("flp: ppq {} tempo {} playlist items {}", ppq, tempo, prj.playlist.len());
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
                let name = if c.name.is_empty() { &c.channel_type } else { &c.name };
                format!("  [{:2}] {}: {}", c.iid, name, w)
            })
            .collect::<Vec<_>>()
            .join("\n");
            bail!("Nothing convertible.\n{}", detail);
        }

        // ── end tick / measure count ─────────────────────────────────────
        let mut end_tick: u32 = 0;
        for (chan, _) in &usable_samples { end_tick = end_tick.max(end_tick_of(&placed, chan.iid)); }
        for (chan, _) in &usable_embedded { end_tick = end_tick.max(end_tick_of(&placed, chan.iid)); }
        for chan in &usable_natives { end_tick = end_tick.max(end_tick_of(&placed, chan.iid)); }
        for chan in &usable_sf2 { end_tick = end_tick.max(end_tick_of(&placed, chan.iid)); }
        let n_measures = std::cmp::max(1, (end_tick + ticks_per_measure - 1) / ticks_per_measure);

        // ── sample files ─────────────────────────────────────────────────
        let mut sample_files: HashMap<u16, String> = HashMap::new();
        let mut sample_meta: Vec<SampleJsonEntry> = Vec::new();
        let mut sample_outs: Vec<SampleOut> = Vec::new();
        let mut styles: HashMap<u16, NoteStyle> = HashMap::new();

        let mut decoded: Vec<(&flp_src::Channel, PathBuf)> = Vec::with_capacity(usable_samples.len());

        for (chan, src) in &usable_samples {
            let raw_name = if chan.name.is_empty() {
                src.file_stem().and_then(|s| s.to_str()).unwrap_or("sample").to_string()
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
                        if chan.name.is_empty() { "<unnamed>" } else { &chan.name },
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
            sample_outs.push(SampleOut { rel_path: rel.clone(), wav: bytes });
            sample_files.insert(chan.iid, rel.clone());
            sample_meta.push(SampleJsonEntry {
                id: chan.iid,
                name: if chan.name.is_empty() { raw_name } else { chan.name.clone() },
                             file: rel,
                             c5_speed: sample_rate,
            });
            decoded.push((*chan, src.clone()));
        }

        let usable_samples = decoded;

        // ── embedded samples ─────────────────────────────────────────────
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

                sample_outs.push(SampleOut { rel_path: rel.clone(), wav: wav.to_vec() });
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
                                   MultiSampleFidelity { files: zone_files, root_keys: zone_roots },
                );
            }
        }

        // ── native + SF2 styles ──────────────────────────────────────────
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

        // ── render parts into a Song ─────────────────────────────────────
        let title = if prj.title.is_empty() {
            src_path.file_stem().and_then(|s| s.to_str()).unwrap_or("(untitled)").to_string()
        } else {
            prj.title.clone()
        };
        let source_name = src_path.file_name().and_then(|s| s.to_str()).unwrap_or("(untitled)");

        let mut song = Song::new(
            &title,
            source_name,
            "flp2trackscript",
            tempo,
            subs,
            (4, 4),
                                 100.0,
                                 "equal_power",
                                 "names",
        );

        let mut part_for_iid: HashMap<u16, usize> = HashMap::new();
        let mut part_name_used: HashMap<String, u32> = HashMap::new();

        // Built once; the measure loop only walks notes.
        let mut channels_here: Vec<(&flp_src::Channel, &'static str)> = Vec::new();
        channels_here.extend(usable_samples.iter().map(|(c, _)| (*c, "sample")));
        channels_here.extend(usable_embedded.iter().map(|(c, _)| (*c, "sample")));
        channels_here.extend(usable_natives.iter().map(|c| (*c, "instrument")));
        channels_here.extend(usable_sf2.iter().map(|c| (*c, "instrument")));

        for m in 0..n_measures {
            let m_start = m * ticks_per_measure;
            let m_end = m_start + ticks_per_measure;

            let mut line = MeasureLine { calls: Vec::new(), extra: Vec::new() };

            for &(chan, kind) in &channels_here {
                let notes_here = notes_in_measure(&placed, chan.iid, m_start, m_end);
                if notes_here.is_empty() {
                    continue;
                }

                let source_path = match kind {
                    "sample" => sample_files.get(&chan.iid).map(|s| s.as_str()).unwrap_or(""),
                    "instrument" => sidecar_path.get(&chan.iid).map(|s| s.as_str()).unwrap_or(""),
                    _ => "",
                };

                let body = render_part(
                    &notes_here,
                    m_start,
                    subs,
                    ppq,
                    styles.get(&chan.iid).copied().unwrap_or(NoteStyle::neutral(tempo)),
                                       source_path,
                );

                let part_idx = match part_for_iid.get(&chan.iid) {
                    Some(&idx) => idx,
                    None => {
                        let raw_name = if chan.name.is_empty() {
                            format!("Ch{:02}", chan.iid)
                        } else {
                            chan.name.clone()
                        };
                        let base = identifier(&raw_name);
                        let count = {
                            let e = part_name_used.entry(base.clone()).or_insert(0);
                            *e += 1;
                            *e
                        };
                        let name = if count == 1 { base } else { format!("{}{:02}", base, count) };
                        let source_arg = crate::spec::source_arg(source_path, None);
                        let idx = song.get_or_create_part(&name, source_arg);
                        part_for_iid.insert(chan.iid, idx);
                        idx
                    }
                };

                let section_idx = song.add_section(part_idx, body);
                line.calls.push(Call { part_idx, section_idx, overrides: None });
            }

            song.measures.push(line);
        }

        // ── post-comments ────────────────────────────────────────────────
        song.post_comments.push(
            "// ------------------------------------------------------------".to_string(),
        );
        song.post_comments.push(format!("// {}", title));
        song.post_comments.push(
            "// converted from FL Studio project by flp2trackscript (rust)".to_string(),
        );
        if !prj.author.is_empty() {
            song.post_comments.push(format!("// author: {}", prj.author));
        }
        if !prj.genre.is_empty() {
            song.post_comments.push(format!("// genre: {}", prj.genre));
        }
        if prj.tempo <= 0.0 {
            song.post_comments.push(format!(
                "//\n// No tempo found in project; using {} BPM.",
                DEFAULT_TEMPO
            ));
        }
        if !used_playlist {
            song.post_comments.push(
                "//\n// No pattern clips found in the playlist: patterns were laid out at their\n\
// own local positions (all patterns overlap from the start).".to_string(),
            );
        }

        if !envelope_only.is_empty() {
            song.post_comments.push(
                "//\n// Channels with active sampler envelopes (parsed, not yet rendered):".to_string(),
            );
            for (iid, name, n) in &envelope_only {
                song.post_comments.push(format!(
                    "//   [{:2}] {}: {} active envelope(s) — attack/decay/sustain/release not applied",
                                                iid, name, n
                ));
            }
        }

        if !usable_embedded.is_empty() {
            song.post_comments.push(
                "//\n// Embedded audio extracted from plugin state:".to_string(),
            );
            for (chan, emb) in &usable_embedded {
                let name = if chan.name.is_empty() { &chan.channel_type } else { &chan.name };
                song.post_comments.push(format!(
                    "//   [{:2}] {}: {} file(s) found, {} zone(s) written to samples.json{}",
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
            song.post_comments.push("//\n// Warnings:".to_string());
            for w in &warnings {
                song.post_comments.push(format!("//   {}", w));
            }
        }
        if !skipped.is_empty() {
            song.post_comments.push("//\n// Skipped:".to_string());
            for (c, w) in &skipped {
                let name = if c.name.is_empty() { &c.channel_type } else { &c.name };
                song.post_comments.push(format!("//   [{:2}] {}: {}", c.iid, name, w));
            }
        }

        let trackscript = song.write();

        let samples_json = serde_json::to_string_pretty(&SamplesJson {
            format: "trackscript-sample-library",
                version: 2,
                samples: sample_meta,
                instruments: vec![],
        })?;

        // ── fidelity sidecar ─────────────────────────────────────────────
        let mut fidelity = build_fidelity(prj, &resolved_map, &sidecar_path);
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
            trackscript,
            samples: sample_outs,
            samples_json,
            aux: aux_files,
        })
}

// ── probe (check) ──────────────────────────────────────────────────────

#[derive(Debug)]
pub struct MissingDep {
    pub channel_iid: u16,
    pub channel_name: String,
    pub kind: DepKind,
    pub reason: String,
}

#[derive(Debug, Clone, Copy)]
pub enum DepKind { Sample, Plugin, Sf2, Unknown }

#[derive(Debug)]
pub struct FallbackUse {
    pub channel_iid: u16,
    pub replaces: String,
    pub used: String,
    pub resolved: bool,
}

#[derive(Debug, Default)]
pub struct ProbeReport {
    pub missing: Vec<MissingDep>,
    pub fallbacks: Vec<FallbackUse>,
    pub warnings: Vec<String>,
}

impl ProbeReport {
    pub fn problem_count(&self) -> usize {
        self.missing.len()
        + self.fallbacks.iter().filter(|f| !f.resolved).count()
    }
    pub fn is_clean(&self) -> bool {
        self.problem_count() == 0 && self.fallbacks.is_empty()
    }
}

pub fn probe(src_path: &Path, opts: &ConvertOptions) -> Result<ProbeReport> {
    let prj = crate::formats::flp::parse_file(src_path)?;
    let flp_dir = src_path.parent().unwrap_or_else(|| Path::new("."));

    let (placed, _, _) = place_notes(&prj);
    let plugin_index = PluginIndex::scan();
    let fallback_table = RegistryTable::load().ok();
    let mut aux_files: Vec<AuxFile> = Vec::new();

    let Classified {
        usable_samples,
        usable_embedded,
        usable_natives,
        usable_sf2,
        resolved_map,
        skipped,
        warnings,
        ..
    } = classify_channels(
        &prj,
        &placed,
        opts,
        flp_dir,
        &plugin_index,
        fallback_table.as_ref(),
                          &mut aux_files,
    );

    let mut used: std::collections::HashSet<u16> = Default::default();
    for (c, _) in &usable_samples  { used.insert(c.iid); }
    for (c, _) in &usable_embedded { used.insert(c.iid); }
    for c in &usable_natives       { used.insert(c.iid); }
    for c in &usable_sf2           { used.insert(c.iid); }

    let mut report = ProbeReport { warnings, ..Default::default() };

    for chan in &prj.channels {
        if used.contains(&chan.iid) { continue; }
        let notes = placed.get(&chan.iid).map_or(0, |v| v.len());
        if notes == 0 { continue; }

        let reason = skipped
        .iter()
        .find(|(c, _)| c.iid == chan.iid)
        .map(|(_, r)| r.clone())
        .unwrap_or_else(|| "channel has notes but was not convertible".into());

        let lower = reason.to_ascii_lowercase();
        let kind = if lower.contains("soundfont") || lower.contains("sf2") {
            DepKind::Sf2
        } else if lower.contains("sample") {
            DepKind::Sample
        } else if lower.contains("plugin") || lower.contains("vst") {
            DepKind::Plugin
        } else {
            DepKind::Unknown
        };

        report.missing.push(MissingDep {
            channel_iid: chan.iid,
            channel_name: if chan.name.is_empty() {
                chan.channel_type.clone()
            } else {
                chan.name.clone()
            },
            kind,
            reason,
        });
    }

    for (iid, res) in &resolved_map {
        if let Resolved::Fallback { name, path, replaces, .. } = res {
            report.fallbacks.push(FallbackUse {
                channel_iid: *iid,
                replaces: replaces.clone(),
                                  used: name.clone(),
                                  resolved: path.is_some(),
            });
        }
    }

    Ok(report)
}

