//! FLP → Halotext converter.

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

use super::{safe_name, AuxFile, ConvertOptions, Converted, SampleJsonEntry, SampleOut, SamplesJson};
use crate::deps::index::PluginIndex;
use crate::deps::registry::RegistryTable;
use crate::fidelity::MultiSampleFidelity;
use crate::formats::flp as flp_src;
use crate::formats::halotext::emit::emit_document_normalized;
use crate::formats::halotext::{
    BlockCall, Config, HalotextFile, HalotextPart, MeasureSelection, ShimEvent, FallbackSpec
};

use classify::{classify_channels, Classified};
use extract::{build_fidelity, extract_inserts};
use place::place_notes;
use render::{render_part, NoteStyle, extract_vol_env, channel_pitch};
use resolve::Resolved;
use wav::{decode_audio_to_wav, wav_sample_rate, wav_secs, crop_wav};

const DEFAULT_FLPPQ: u32 = 96;
const DEFAULT_TEMPO: f64 = 120.0;
pub(super) const PLAY_FULL_SAMPLE: bool = true;
pub(super) const MAX_FULL_BEATS: f64 = 4.0;
pub(super) const MAX_NOTE_TICKS: u32 = 500_000;
pub(super) const MIN_EMBEDDED_BYTES: usize = 512;
pub(super) const FL_C5_MIDI: i32 = 60;
pub(super) const FL_C5_KEY: u16 = 60;
pub(super) const SF2_STATE_MAX: usize = 4096;

pub(super) fn debug_on() -> bool { std::env::var_os("FLP_DEBUG").is_some() }
pub(super) fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{:02x}", x)).collect::<Vec<_>>().join(" ") }

fn ensure_vst3(s: &str) -> String {
    if s.is_empty() || s.ends_with(".vst3") || s.ends_with(".sf2") || s.ends_with(".wav") {
        s.to_string()
    } else {
        format!("{}.vst3", s)
    }
}

fn build_channel_to_insert(prj: &flp_src::Project) -> HashMap<u16, u8> {
    let mut out = HashMap::new();
    for chan in &prj.channels {
        if chan.insert > 0 && chan.insert <= 255 { out.insert(chan.iid, chan.insert as u8); }
    }
    out
}

/// Read an `.instrument` sidecar and extract the underlying SF2 path + preset.
fn read_sf2_sidecar(path: &str, aux_files: &[AuxFile]) -> Option<(String, i16, i16)> {
    let aux = aux_files.iter().find(|a| a.rel_path == path)?;
    let text = std::str::from_utf8(&aux.bytes).ok()?;
    if !text.lines().any(|l| l.trim() == "Kind: sf2;") {
        return None;
    }
    let file = text.lines()
    .find(|l| l.trim().starts_with("File:"))
    .and_then(|l| l.split('\'').nth(1))?
    .to_string();
    let bank = text.lines()
    .find(|l| l.trim().starts_with("Bank:"))
    .and_then(|l| l.split(':').nth(1))
    .and_then(|s| s.trim().trim_end_matches(';').parse::<i16>().ok())
    .unwrap_or(0);
    let preset = text.lines()
    .find(|l| l.trim().starts_with("Preset:"))
    .and_then(|l| l.split(':').nth(1))
    .and_then(|s| s.trim().trim_end_matches(';').parse::<i16>().ok())
    .unwrap_or(0);
    Some((file, bank, preset))
}

pub(super) fn convert(prj: &flp_src::Project, src_path: &Path, opts: &ConvertOptions) -> Result<Converted> {
    let subs = opts.subdivisions;
    let ppq = if prj.ppq == 0 { DEFAULT_FLPPQ } else { prj.ppq as u32 };
    let ticks_per_measure = ppq * 4;
    let flp_dir = src_path.parent().unwrap_or_else(|| Path::new("."));
    let tempo = if prj.tempo > 0.0 { prj.tempo } else { DEFAULT_TEMPO };

    let (placed, _used_playlist, dropped_notes) = place_notes(prj);
    let plugin_index = PluginIndex::scan();
    let fallback_table = match RegistryTable::load() {
        Ok(t) => Some(t),
        Err(e) => { eprintln!("flp: fallback table unavailable ({e:#})"); None }
    };

    let mut aux_files: Vec<AuxFile> = Vec::new();
    let mut multi_zones: BTreeMap<String, MultiSampleFidelity> = BTreeMap::new();
    let mut warnings: Vec<String> = Vec::new();
    if dropped_notes > 0 { warnings.push(format!("{} note(s) dropped as misdecoded", dropped_notes)); }

    let inserts = extract_inserts(prj, &plugin_index, fallback_table.as_ref(), &mut aux_files);
    let channel_to_insert = build_channel_to_insert(prj);
    let shims_by_channel = place::channel_shims(&inserts, &channel_to_insert);

    let Classified {
        usable_samples, usable_embedded, usable_natives, usable_sf2,
        resolved_map, sidecar_path, skipped, warnings: cls_warnings, envelope_only,
    } = classify_channels(prj, &placed, opts, flp_dir, &plugin_index, fallback_table.as_ref(), &mut aux_files);
    warnings.extend(cls_warnings);

    if usable_samples.is_empty() && usable_embedded.is_empty() && usable_natives.is_empty() && usable_sf2.is_empty() {
        bail!("Nothing convertible.");
    }

    let mut end_tick: u32 = 0;
    for (_, plist) in &placed {
        for p in plist { end_tick = end_tick.max(p.abs.saturating_add(p.note.length)); }
    }
    let n_measures = std::cmp::max(1, (end_tick + ticks_per_measure - 1) / ticks_per_measure);

    let mut sample_meta: Vec<SampleJsonEntry> = Vec::new();
    let mut sample_outs: Vec<SampleOut> = Vec::new();
    let mut styles: HashMap<u16, NoteStyle> = HashMap::new();
    let mut sample_files: HashMap<u16, String> = HashMap::new();

    // ── sample extraction ──
    let mut decoded: Vec<(&flp_src::Channel, PathBuf)> = Vec::with_capacity(usable_samples.len());
    for (chan, src) in &usable_samples {
        let raw_name = if chan.name.is_empty() { src.file_stem().and_then(|s| s.to_str()).unwrap_or("sample").to_string() } else { chan.name.clone() };
        let target_name = format!("{:02}_{}.wav", chan.iid, safe_name(&raw_name));
        let rel = format!("samples/{}", target_name);

        let mut bytes = match decode_audio_to_wav(src) { Ok(b) => b, Err(_) => continue };
        let sample_rate = wav_sample_rate(&bytes).unwrap_or(44100);
        let total_samples = (wav_secs(&bytes).unwrap_or(0.0) * sample_rate as f64) as u32;
        let mut start_s = chan.sample_start.unwrap_or(0);
        let mut end_s = chan.sample_end.unwrap_or(total_samples);
        if end_s == 0 || end_s > total_samples { end_s = total_samples; }
        if start_s > 0 || end_s < total_samples {
            if let Ok(cropped) = crop_wav(&bytes, start_s, end_s) { bytes = cropped; start_s = 0; }
        }

        let (pitch_semis, _) = channel_pitch(chan);
        styles.insert(chan.iid, NoteStyle { transpose: pitch_semis, full_secs: wav_secs(&bytes), bpm: tempo, vol_scale: chan.volume_scale(), pan_offset: chan.pan_norm(), sample_rate, sample_start: start_s, vol_env: extract_vol_env(chan) });

        sample_files.insert(chan.iid, target_name.clone());

        sample_outs.push(SampleOut { rel_path: rel.clone(), wav: bytes });
        sample_meta.push(SampleJsonEntry { id: chan.iid, name: if chan.name.is_empty() { raw_name } else { chan.name.clone() }, file: rel, c5_speed: sample_rate });
        decoded.push((*chan, src.clone()));
    }
    let usable_samples = decoded;

    for (chan, emb) in &usable_embedded {
        let raw_name = if chan.name.is_empty() { format!("Ch{:02}", chan.iid) } else { chan.name.clone() };
        let safe = safe_name(&raw_name);
        let primary_rel = format!("samples/{:02}_{}.wav", chan.iid, safe);
        let primary_target = format!("{:02}_{}.wav", chan.iid, safe);
        let sample_rate = wav_sample_rate(&emb.wav).unwrap_or(44100);
        let (pitch_semis, _) = channel_pitch(chan);

        styles.insert(chan.iid, NoteStyle { transpose: emb.root_key.map_or(0, |r| FL_C5_MIDI - r as i32) + pitch_semis, full_secs: wav_secs(&emb.wav), bpm: tempo, vol_scale: chan.volume_scale(), pan_offset: chan.pan_norm(), sample_rate, sample_start: chan.sample_start.unwrap_or(0), vol_env: extract_vol_env(chan) });

        sample_files.insert(chan.iid, primary_target);

        let mut zone_files: Vec<String> = Vec::new();
        let mut zone_roots: Vec<Option<i32>> = Vec::new();
        for (zi, (wav, root)) in emb.all_zones.iter().enumerate() {
            let rel = if zi == 0 { primary_rel.clone() } else { format!("samples/{:02}_{}_zone{:02}.wav", chan.iid, safe, zi) };
            let zone_id: u16 = if zi == 0 { chan.iid } else { (20000u32 + (chan.iid as u32) * 10 + zi as u32) as u16 };
            let zone_name = if zi == 0 { raw_name.clone() } else { format!("{}_zone{:02}", raw_name, zi) };
            let zone_rate = wav_sample_rate(wav).unwrap_or(sample_rate);
            sample_outs.push(SampleOut { rel_path: rel.clone(), wav: wav.to_vec() });
            sample_meta.push(SampleJsonEntry { id: zone_id, name: zone_name, file: rel.clone(), c5_speed: zone_rate });
            zone_files.push(rel);
            zone_roots.push(root.map(|r| r as i32));
        }
        if !zone_files.is_empty() { multi_zones.insert(chan.iid.to_string(), MultiSampleFidelity { files: zone_files, root_keys: zone_roots }); }
    }

    for chan in usable_natives.iter().chain(usable_sf2.iter()) {
        let gain = match resolved_map.get(&chan.iid) { Some(Resolved::Fallback { gain, .. }) => *gain, _ => 1.0 };
        styles.insert(chan.iid, NoteStyle { vol_scale: chan.volume_scale() * gain as f64, pan_offset: chan.pan_norm(), ..NoteStyle::neutral(tempo) });
    }

    // ── assemble HalotextFile ──
    let title = if prj.title.is_empty() { src_path.file_stem().and_then(|s| s.to_str()).unwrap_or("(untitled)").to_string() } else { prj.title.clone() };
    let source_name = src_path.file_name().and_then(|s| s.to_str()).unwrap_or("(untitled)");

    let mut file = HalotextFile::default();
    file.config = Config {
        bpm: tempo, subdivisions: subs, time_signature: (4, 4), song_volume: 100.0, panning_law: "equal_power".into(), stereo_width: 1.0,
        raw: [
            ("TrackscriptVersion".to_string(), "1.10".to_string()),
            ("Overlap".to_string(), "stack".to_string()),
            ("PitchDefault".to_string(), "names".to_string()),
        ].into_iter().collect(),
    };

    let mut channels_here: Vec<(&flp_src::Channel, &'static str)> = Vec::new();
    channels_here.extend(usable_samples.iter().map(|(c, _)| (*c, "sample")));
    channels_here.extend(usable_embedded.iter().map(|(c, _)| (*c, "sample")));
    channels_here.extend(usable_natives.iter().map(|c| (*c, "instrument")));
    channels_here.extend(usable_sf2.iter().map(|c| (*c, "instrument")));

    let mut part_names = HashMap::new();
    let mut name_counts = HashMap::new();

    // 1. Create parts
    for &(chan, _) in &channels_here {
        let raw_name = if chan.name.is_empty() { format!("Ch{:02}", chan.iid) } else { chan.name.clone() };
        let base: String = raw_name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
        let count = name_counts.entry(base.clone()).or_insert(0);
        *count += 1;
        let name = if *count == 1 { base } else { format!("{}{:02}", base, count) };
        part_names.insert(chan.iid, name.clone());

        let mut preset: Option<i16> = None;
        let mut bank: Option<i16> = None;

        let source_path: String = if let Some(s) = sample_files.get(&chan.iid) {
            s.clone()
        } else if let Some(Resolved::Fallback { replaces, .. }) = resolved_map.get(&chan.iid) {
            ensure_vst3(replaces)
        } else if let Some(p) = &chan.plugin {
            if sf2::is_sf2_player(p) {
                if let Some(sf) = sf2::parse_sf2_state(&p.state) {
                    preset = Some(sf.preset as i16);
                    bank = Some(sf.bank as i16);
                    let filename = sf.path.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(&sf.path);
                    format!("samples/{}", safe_name(&filename))
                } else if let Some(path) = sidecar_path.get(&chan.iid) {
                    if let Some((sf2_path, sf_bank, sf_preset)) = read_sf2_sidecar(path, &aux_files) {
                        bank = Some(sf_bank);
                        preset = Some(sf_preset);
                        sf2_path
                    } else {
                        path.clone()
                    }
                } else {
                    String::new()
                }
            } else if let Some(path) = sidecar_path.get(&chan.iid) {
                if let Some((sf2_path, sf_bank, sf_preset)) = read_sf2_sidecar(path, &aux_files) {
                    bank = Some(sf_bank);
                    preset = Some(sf_preset);
                    sf2_path
                } else {
                    path.clone()
                }
            } else {
                let clean: String = p.display_name.chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
                .collect();
                ensure_vst3(clean.trim_matches('_'))
            }
        } else if let Some(path) = sidecar_path.get(&chan.iid) {
            if let Some((sf2_path, sf_bank, sf_preset)) = read_sf2_sidecar(path, &aux_files) {
                bank = Some(sf_bank);
                preset = Some(sf_preset);
                sf2_path
            } else {
                path.clone()
            }
        } else {
            String::new()
        };

        let part = HalotextPart {
            name: name.clone(), sample_path: source_path, beats_per_measure: 4, subs_per_beat: subs, len_beats: None,
            sample_offset: 0, preset, bank, src_bpm: None,
            events: Vec::new(), measures: Vec::new(), section_lengths: Vec::new(),
            default_vol: None, default_pan: None, default_dur: None,
                shims: Vec::new(),
        };
        file.parts.insert(name, part);
    }

    // 2. Build BlockList
    for m in 0..n_measures {
        let m_start = m * ticks_per_measure;
        let m_end = m_start + ticks_per_measure;
        let mut line = Vec::new();

        for &(chan, _) in &channels_here {
            let notes_here = place::notes_in_measure(&placed, chan.iid, m_start, m_end);
            if notes_here.is_empty() { continue; }

            let sref = sample_files.get(&chan.iid).cloned().unwrap_or_default();
            let style = styles.get(&chan.iid).copied().unwrap_or(NoteStyle::neutral(tempo));
            let events = render_part(&notes_here, m_start, subs, ppq, style, &sref);

            if !events.is_empty() {
                let name = part_names.get(&chan.iid).unwrap();
                let part = file.parts.get_mut(name).unwrap();
                let section_idx = part.measures.len();
                part.measures.push(events);
                part.section_lengths.push((subs * 4) as f64);

                line.push(BlockCall {
                    name: name.clone(),
                          selections: vec![MeasureSelection { measure: section_idx, overrides: None }],
                          bypass_shims: false,
                });
            }
        }
        line.sort_by(|a, b| a.name.cmp(&b.name));
        file.block_list.push(line);
    }

    // 3. Flatten events
    for part in file.parts.values_mut() {
        let mut all_events = Vec::new();
        let mut offset = 0.0;
        for (i, measure_events) in part.measures.iter().enumerate() {
            for ev in measure_events { let mut e = ev.clone(); e.row += offset; all_events.push(e); }
            offset += part.section_lengths[i];
        }
        part.events = all_events;
    }

    // 4. Shims live on the part declaration.
    for (iid, name) in &part_names {
        let Some(chain) = shims_by_channel.get(iid) else { continue };
        if let Some(part) = file.parts.get_mut(name) {
            part.shims = chain
            .iter()
            .map(|slug| ShimEvent {
                line: 0, row: 0.0, target: slug.clone(), params: vec![], length_rows: None,
                 mix: 1.0, gain: 100.0, pan: 0.0, attached_call: None, waypoints: vec![],
                 property: String::new(), start_value: 0.0, op: String::new(),
                 amount: 0.0, interval_rows: 0.0, count: None,
            })
            .collect();
        }
    }

    // 5. Fallbacks
    for res in resolved_map.values() {
        if let Resolved::Fallback { name, replaces, gain, .. } = res {
            let intent = ensure_vst3(replaces);
            let actual = ensure_vst3(name);
            file.fallbacks.insert(intent, FallbackSpec { use_: actual, gain: Some(*gain as f64) });
        }
    }

    let mut halotext = emit_document_normalized(&mut file);

    // Append comments directly to the output string
    halotext.push_str("\n// ------------------------------------------------------------\n");
    halotext.push_str(&format!("// {}\n", title));
    halotext.push_str(&format!("// converted from FL Studio project ({}) by flp2halotext\n", source_name));
    if !warnings.is_empty() {
        halotext.push_str("//\n// Warnings:\n");
        for w in &warnings { halotext.push_str(&format!("//   {}\n", w)); }
    }
    if !skipped.is_empty() {
        halotext.push_str("//\n// Skipped:\n");
        for (c, w) in &skipped {
            let name = if c.name.is_empty() { &c.channel_type } else { &c.name };
            halotext.push_str(&format!("//   [{:2}] {}: {}\n", c.iid, name, w));
        }
    }
    let _ = envelope_only;

    let samples_json = serde_json::to_string_pretty(&SamplesJson {
        format: "halotext-sample-library", version: 2, samples: sample_meta, instruments: vec![],
    })?;

    let mut fidelity = build_fidelity(prj, &resolved_map, &HashMap::new());
    fidelity.multi_sample = multi_zones;
    fidelity.inserts = inserts;

    aux_files.retain(|a| {
        let p = a.rel_path.as_str();
        let is_instrument_sidecar = p.starts_with("instruments/") && p.ends_with(".instrument");
        !is_instrument_sidecar && !p.starts_with("inserts/") && p != crate::fidelity::FIDELITY_FILENAME
    });

    Ok(Converted { halotext, samples: sample_outs, samples_json, aux: aux_files })
}

// probe() left exactly as it was...
pub fn probe(src_path: &Path, opts: &ConvertOptions) -> Result<ProbeReport> {
    let prj = crate::formats::flp::parse_file(src_path)?;
    let flp_dir = src_path.parent().unwrap_or_else(|| Path::new("."));
    let (placed, _, _) = place_notes(&prj);
    let plugin_index = PluginIndex::scan();
    let fallback_table = RegistryTable::load().ok();
    let mut aux_files: Vec<AuxFile> = Vec::new();

    let Classified { usable_samples, usable_embedded, usable_natives, usable_sf2, resolved_map, skipped, warnings, .. } =
    classify_channels(&prj, &placed, opts, flp_dir, &plugin_index, fallback_table.as_ref(), &mut aux_files);

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

        let reason = skipped.iter().find(|(c, _)| c.iid == chan.iid).map(|(_, r)| r.clone())
        .unwrap_or_else(|| "channel has notes but was not convertible".into());
        let lower = reason.to_ascii_lowercase();
        let kind = if lower.contains("soundfont") || lower.contains("sf2") { DepKind::Sf2 }
        else if lower.contains("sample") { DepKind::Sample }
        else if lower.contains("plugin") || lower.contains("vst") || lower.contains("fallback") || lower.contains("not installed") { DepKind::Plugin }
        else { DepKind::Unknown };

        report.missing.push(MissingDep { channel_iid: chan.iid, channel_name: if chan.name.is_empty() { chan.channel_type.clone() } else { chan.name.clone() }, kind, reason });
    }
    for (iid, res) in &resolved_map {
        if let Resolved::Fallback { name, path, replaces, via_default, .. } = res {
            report.fallbacks.push(FallbackUse { channel_iid: *iid, replaces: replaces.clone(), used: name.clone(), resolved: path.is_some() && !*via_default, via_default: *via_default });
        }
    }
    Ok(report)
}

#[derive(Debug)]
pub struct MissingDep { pub channel_iid: u16, pub channel_name: String, pub kind: DepKind, pub reason: String }
#[derive(Debug, Clone, Copy)]
pub enum DepKind { Sample, Plugin, Sf2, Unknown }
#[derive(Debug)]
pub struct FallbackUse { pub channel_iid: u16, pub replaces: String, pub used: String, pub resolved: bool, pub via_default: bool }
#[derive(Debug, Default)]
pub struct ProbeReport { pub missing: Vec<MissingDep>, pub fallbacks: Vec<FallbackUse>, pub warnings: Vec<String> }
impl ProbeReport {
    pub fn problem_count(&self) -> usize { self.missing.len() + self.fallbacks.iter().filter(|f| !f.resolved).count() }
    pub fn is_clean(&self) -> bool { self.problem_count() == 0 && self.fallbacks.is_empty() }
}
