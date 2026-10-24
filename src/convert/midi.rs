// src/convert/midi.rs
//! MIDI → Trackscript converter.
//!
//! Multi-SF2 aware.  If the source file's directory contains several
//! SoundFonts — one per instrument, as game-rip archives often do —
//! each MIDI channel is matched to the SF2 whose preset table
//! contains that channel's `(bank, program)`.  Channels with no match
//! fall back to a default bank; if there's no default either, the
//! channel is skipped with a warning.
//!
//! When every channel resolves to the same SF2 (a plain GM MIDI), the
//! trackscript writes `SoundFont: ask;` so the launcher can offer to
//! override it at play time.  When channels resolve to different
//! SF2s (a game-rip archive), the per-channel mapping is meaningful
//! and no prompt is written.
//!
//! SF2 discovery consults, in order: the source file's directory,
//! `--samples-dir`, `$XDG_DATA_HOME/circolate/soundfonts`, and
//! `/usr/share/circolate/soundfonts`.
//!
//! Structure follows FLP and IT: build a `Song` and let it write.
//! Note emission goes through `spec::note` so the syntax matches the
//! other converters byte for byte.

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use super::song::{Call, MeasureLine, Song};
use super::{fmt, note_name, pos_str, safe_name, AuxFile, ConvertOptions, Converted};
use crate::hosting::sf2::sfl2::hi::SoundFont;
use crate::hosting::sf2::sfl2::lo::Sf2Data;
use crate::spec;
use crate::formats::midi::{MidiChannel, MidiFile, MidiNote};

/// Files bigger than this are assumed to be general-purpose GM banks
/// and skipped during per-channel matching.  A single-instrument SF2
/// is usually under a few hundred KB; a full GM bank is 10–200 MB.
/// Scanning a 148 MB FluidR3 for every export is not worth it when
/// the user can pass it via `--sf2` explicitly.
const PRESET_SCAN_MAX_BYTES: u64 = 30 * 1024 * 1024;

pub(super) fn convert(
    mf: &MidiFile,
    src_path: &Path,
    opts: &ConvertOptions,
) -> Result<Converted> {
    let mut aux: Vec<AuxFile> = Vec::new();
    let mut sf2_copied: HashMap<PathBuf, String> = HashMap::new();

    let (per_channel, default_sf2) = if opts.sf2_path.is_some() {
        (HashMap::new(), opts.sf2_path.clone())
    } else {
        let dirs = sf2_search_dirs(opts, src_path);
        let preset_map = scan_preset_tables(&dirs);
        let default = find_default_sf2(opts, src_path).ok();
        (preset_map, default)
    };

    let mut distinct_sf2s: HashSet<PathBuf> = HashSet::new();
    let mut channel_sf2: Vec<(u8, u8, u8, String)> = Vec::new();

    for ch in &mf.channels {
        let (bank, patch) = bank_patch_for(ch);

        let mut chosen = per_channel.get(&(bank, patch)).cloned();
        if chosen.is_none() && ch.channel == 9 {
            chosen = per_channel.get(&(0, 0)).cloned();
        }
        let chosen = chosen.or_else(|| default_sf2.clone());

        let Some(sf2_path) = chosen else {
            eprintln!(
                "midi: channel {} (bank {} patch {}) has no matching SF2; skipping",
                      ch.channel, bank, patch
            );
            continue;
        };

        let rel = copy_sf2_once(&sf2_path, &mut sf2_copied, &mut aux)?;
        distinct_sf2s.insert(sf2_path);
        channel_sf2.push((ch.channel, bank, patch, rel));
    }

    if channel_sf2.is_empty() {
        bail!("no SoundFonts matched any channel; cannot convert");
    }

    for (channel, bank, patch, sf2_rel) in &channel_sf2 {
        let Some(ch) = mf.channels.iter().find(|c| c.channel == *channel) else {
            continue;
        };
        let part_name = part_name_for(ch);
        write_sidecar(&mut aux, &part_name, sf2_rel, *bank, *patch, *channel);
    }

    let (num, den) = mf.time_sig;
    let subs = opts.subdivisions;
    let measure_len_beats = (num as f64) * 4.0 / (den as f64);
    if measure_len_beats <= 0.0 {
        bail!("invalid time signature {num}/{den}");
    }

    let title = src_path
    .file_stem()
    .and_then(|s| s.to_str())
    .unwrap_or("(untitled)")
    .to_string();
    let source_name = src_path
    .file_name()
    .and_then(|s| s.to_str())
    .unwrap_or("(untitled)");

    let mut song = Song::new(
        &title,
        source_name,
        "midi2trackscript",
        mf.initial_bpm,
        subs,
        (num as u32, den as u32),
                             100.0,
                             "equal_power",
                             "names",
    );

    if distinct_sf2s.len() == 1 {
        song.soundfont = Some("ask".to_string());
    }

    let mut last_beat: f64 = 0.0;
    for ch in &mf.channels {
        for n in &ch.notes {
            let end = n.start_beat + n.dur_beat;
            if end > last_beat {
                last_beat = end;
            }
        }
    }
    let n_measures = ((last_beat / measure_len_beats).ceil() as usize).max(1);

    for mi in 0..n_measures {
        let m_start = (mi as f64) * measure_len_beats;
        let m_end = m_start + measure_len_beats;

        let mut line = MeasureLine { calls: Vec::new(), extra: Vec::new() };

        for ch in &mf.channels {
            let mnotes: Vec<&MidiNote> = ch
            .notes
            .iter()
            .filter(|n| n.start_beat >= m_start && n.start_beat < m_end)
            .collect();
            if mnotes.is_empty() {
                continue;
            }

            let part_name = part_name_for(ch);
            let inst_rel = format!("instruments/{}.instrument", part_name);
            let source_arg = spec::source_arg(&inst_rel, None);
            let part_idx = song.get_or_create_part(&part_name, source_arg);

            let body = emit_measure_body(&mnotes, m_start, subs);
            let section_idx = song.add_section(part_idx, body);
            line.calls.push(Call { part_idx, section_idx, overrides: None });
        }

        song.measures.push(line);
    }

    song.post_comments.push(
        "// ------------------------------------------------------------".to_string(),
    );
    song.post_comments.push(format!("// {}", title));
    song.post_comments.push(
        "// converted from Standard MIDI File by midi2trackscript".to_string(),
    );
    song.post_comments.push(format!("// source: {source_name}"));
    song.post_comments.push(format!(
        "// ppq {}, {} channel(s), {} measure(s)",
                                    mf.ppq,
                                    mf.channels.len(),
                                    n_measures
    ));
    if distinct_sf2s.len() == 1 {
        song.post_comments.push(format!(
            "// soundfont: {}",
            distinct_sf2s.iter().next().unwrap().display()
        ));
    } else {
        song.post_comments.push(format!(
            "// soundfonts: {} distinct file(s) matched per channel",
                                        distinct_sf2s.len()
        ));
    }

    let trackscript = song.write();

    Ok(Converted {
        trackscript,
       samples: Vec::new(),
       samples_json: String::new(),
       aux,
    })
}

// ── per-channel helpers ────────────────────────────────────────────────

fn part_name_for(ch: &MidiChannel) -> String {
    if ch.channel == 9 {
        return "Drums".into();
    }
    if let Some(n) = &ch.name {
        let cleaned = safe_name(n);
        if !cleaned.is_empty() && cleaned != "part" {
            return format!("{}_{:02}", cleaned, ch.channel);
        }
    }
    format!("Ch{:02}", ch.channel)
}

fn bank_patch_for(ch: &MidiChannel) -> (u8, u8) {
    if ch.channel == 9 {
        return (128, 0);
    }
    let bank = (((ch.bank_msb as u16) << 7) | (ch.bank_lsb as u16)).min(127) as u8;
    (bank, ch.program)
}

fn write_sidecar(
    aux: &mut Vec<AuxFile>,
    part: &str,
    sf2_rel: &str,
    bank: u8,
    patch: u8,
    channel: u8,
) {
    let inst_rel = format!("instruments/{part}.instrument");
    let text = format!(
        "Instrument {part}\n{{\n    \
Kind: sf2;\n    \
File: '{sf2_rel}';\n    \
Bank: {bank};\n    \
Preset: {patch};\n    \
SourceChannel: {channel};\n\
}}\n"
    );
    aux.push(AuxFile { rel_path: inst_rel, bytes: text.into_bytes() });
}

// ── measure body ───────────────────────────────────────────────────────

fn emit_measure_body(notes: &[&MidiNote], m_start_beat: f64, subs: u32) -> String {
    let mut out = String::new();
    let mut first = true;
    for n in notes {
        if !first {
            out.push(',');
        }
        first = false;

        let local_beat = n.start_beat - m_start_beat;
        let pos = pos_str(local_beat * subs as f64, subs);
        let pitch = note_name(n.key as i32);
        let vol = fmt((n.velocity as f64) * 100.0 / 127.0, 1);
        let dur = fmt(if n.dur_beat > 0.0 { n.dur_beat } else { 0.25 }, 3);

        out.push_str(&spec::note(&pos, &pitch, &dur, &vol, ""));
    }
    out
}

// ── multi-SF2 matching ─────────────────────────────────────────────────

/// Where to look for per-instrument SF2s.  Source directory first,
/// then any user-supplied `--samples-dir`, then the user's
/// downloaded soundfonts, then the shipped defaults.
fn sf2_search_dirs(opts: &ConvertOptions, src_path: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(parent) = src_path.parent() {
        dirs.push(parent.to_path_buf());
    }
    if let Some(d) = &opts.samples_dir {
        dirs.push(d.clone());
    }
    dirs.push(crate::paths::user_data().join("soundfonts"));
    dirs.push(crate::paths::system_data().join("soundfonts"));
    dirs.retain(|d| d.is_dir());
    dirs.sort();
    dirs.dedup();
    dirs
}

fn collect_sf2(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 { return; }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
        if md.file_type().is_dir() {
            collect_sf2(&p, depth - 1, out);
        } else if md.file_type().is_file()
            && p.extension().and_then(|s| s.to_str())
            .map_or(false, |x| x.eq_ignore_ascii_case("sf2"))
            {
                if md.len() > PRESET_SCAN_MAX_BYTES { continue; }
                out.push(p);
            }
    }
}

/// Read every small `.sf2` in `dirs` and build a `(bank, program) ->
/// path` map.  First match wins, so earlier directories take priority.
fn scan_preset_tables(dirs: &[PathBuf]) -> HashMap<(u8, u8), PathBuf> {
    let mut files = Vec::new();
    for d in dirs {
        collect_sf2(d, 3, &mut files);
    }
    files.sort();
    files.dedup();

    let mut map: HashMap<(u8, u8), PathBuf> = HashMap::new();
    for path in files {
        let progress = Arc::new(AtomicU32::new(0));
        let data = match Sf2Data::load_from_file(path.clone(), false, progress) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("midi: skipping {} (load failed: {:#})", path.display(), e);
                continue;
            }
        };
        let sf = SoundFont::from_sf2_data(data);
        for p in &sf.presets {
            let key = ((p.bank & 0xFF) as u8, (p.preset & 0x7F) as u8);
            map.entry(key).or_insert_with(|| path.clone());
        }
    }
    map
}

fn copy_sf2_once(
    src: &Path,
    copied: &mut HashMap<PathBuf, String>,
    aux: &mut Vec<AuxFile>,
) -> Result<String> {
    if let Some(rel) = copied.get(src) {
        return Ok(rel.clone());
    }
    let bytes = std::fs::read(src)
    .with_context(|| format!("reading {}", src.display()))?;
    let stem = src.file_stem()
    .and_then(|s| s.to_str())
    .map(safe_name)
    .unwrap_or_else(|| "sf2".into());
    let mut rel = format!("samples/{}.sf2", stem);
    while copied.values().any(|v| *v == rel) {
        rel = format!("samples/{}_{}.sf2", stem, copied.len());
    }
    aux.push(AuxFile { rel_path: rel.clone(), bytes });
    copied.insert(src.to_path_buf(), rel.clone());
    Ok(rel)
}

// ── default SF2 search ─────────────────────────────────────────────────

fn find_default_sf2(opts: &ConvertOptions, src_path: &Path) -> Result<PathBuf> {
    if let Some(p) = &opts.sf2_path {
        if p.is_file() {
            return Ok(p.clone());
        }
        bail!("--sf2: {} is not a file", p.display());
    }

    let system_sf = crate::paths::system_data().join("soundfonts");
    let user_sf   = crate::paths::user_data().join("soundfonts");

    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(d) = &opts.samples_dir {
        candidates.push(d.join("soundfonts").join("gm.sf2"));
        candidates.push(d.join("gm.sf2"));
    }
    if let Some(parent) = src_path.parent() {
        candidates.push(parent.join("soundfonts").join("gm.sf2"));
        candidates.push(parent.join("gm.sf2"));
    }
    candidates.push(user_sf.join("gm.sf2"));
    candidates.push(system_sf.join("gm.sf2"));
    candidates.push(
        system_sf
        .join("github-collection")
        .join("SF2")
        .join("GM")
        .join("GeneralUser.sf2"),
    );

    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }

    bail!(
        "no SoundFont found for MIDI conversion; drop a gm.sf2 in {}\n\
(tried: {})",
          user_sf.display(),
          candidates
          .iter()
          .map(|p| p.display().to_string())
          .collect::<Vec<_>>()
          .join(", ")
    )
}
