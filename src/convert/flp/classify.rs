// src/convert/flp/classify.rs
//! Classify each FLP channel: usable sample, embedded sample, SF2, VST,
//! or nothing.  Writes VST sidecars and SF2 file copies as a side effect
//! of classification, since those are discovered along the way.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::convert::{identifier, safe_name, AuxFile, ConvertOptions};
use crate::deps::registry::RegistryTable;
use crate::fidelity::FIDELITY_FILENAME;
use crate::deps::index::PluginIndex;
use crate::formats::flp::{self as flp_src, Channel};
use crate::formats::flp_audio;

use super::place::Placed;
use super::render::EmbeddedSample;
use super::resolve::{resolve_plugin, vst_sidecar, Resolved};
use super::sf2::{self, SfState};
use super::slicex;
use super::wav::find_sample;
use super::{debug_on, hex, MIN_EMBEDDED_BYTES, SF2_STATE_MAX};

pub(super) struct Classified<'a> {
    pub(super) usable_samples: Vec<(&'a Channel, PathBuf)>,
    pub(super) usable_embedded: Vec<(&'a Channel, EmbeddedSample)>,
    pub(super) usable_natives: Vec<&'a Channel>,
    pub(super) usable_sf2: Vec<&'a Channel>,
    pub(super) resolved_map: HashMap<u16, Resolved>,
    pub(super) sidecar_path: HashMap<u16, String>,
    pub(super) skipped: Vec<(&'a Channel, String)>,
    pub(super) warnings: Vec<String>,
    pub(super) envelope_only: Vec<(u16, String, usize)>,
}

/// What happened when we tried to handle an SF2 channel.
enum Sf2Outcome {
    Done(String),
    PresetMissing(String),
    Skip(String),
}

pub(super) fn classify_channels<'a>(
    prj: &'a flp_src::Project,
    placed: &HashMap<u16, Vec<Placed>>,
    opts: &ConvertOptions,
    flp_dir: &Path,
    plugin_index: &PluginIndex,
    fallback_table: Option<&RegistryTable>,
    aux_files: &mut Vec<AuxFile>,
) -> Classified<'a> {
    let mut usable_samples: Vec<(&Channel, PathBuf)> = Vec::new();
    let mut usable_embedded: Vec<(&Channel, EmbeddedSample)> = Vec::new();
    let mut usable_natives: Vec<&Channel> = Vec::new();
    let mut usable_sf2: Vec<&Channel> = Vec::new();
    let mut resolved_map: HashMap<u16, Resolved> = HashMap::new();
    let mut skipped: Vec<(&Channel, String)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut envelope_only: Vec<(u16, String, usize)> = Vec::new();
    let mut sf2_copied: HashMap<PathBuf, String> = HashMap::new();
    let mut sidecar_path: HashMap<u16, String> = HashMap::new();
    let mut sf2_preset_cache: HashMap<PathBuf, Option<Vec<(u16, u16)>>> = HashMap::new();

    for chan in &prj.channels {
        if !chan.enabled.unwrap_or(true) {
            skipped.push((chan, "disabled in FL".into()));
            continue;
        }

        let active_envs = chan.envelopes.iter().filter(|e| e.env_enabled != 0).count();
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
            || chan.plugin.as_ref().map_or(false, |p| !p.state.is_empty());
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
                // FL often wraps Slicex channels as "instrument" rather than
                // "native" depending on the FL version or how the project was
                // saved. Check for Slicex here first, so we parse the slice
                // points instead of copying the raw WAV and ignoring them.
                if let Some(plugin) = chan.plugin.as_ref() {
                    if slicex::is_slicex(plugin) {
                        match slicex::convert(chan, plugin, placed, aux_files, &mut warnings, if prj.tempo > 0.0 { prj.tempo } else { super::DEFAULT_TEMPO }) {
                            Ok(out) => {
                                let (lo, hi) = (out.base_key, out.base_key + out.slices as u32);
                                let mut stray_keys: Vec<u32> =
                                placed.get(&chan.iid).map_or_else(Vec::new, |v| {
                                    v.iter()
                                    .map(|p| p.note.key as u32)
                                    .filter(|&k| k < lo || k >= hi)
                                    .collect()
                                });
                                let stray = stray_keys.len();
                                stray_keys.sort_unstable();
                                stray_keys.dedup();
                                if stray > 0 {
                                    warnings.push(format!(
                                        "[{:2}] Slicex: {} note(s) outside slice keys {}..{} will be silent (keys: {})",
                                                          chan.iid,
                                                          stray,
                                                          lo,
                                                          hi - 1,
                                                          stray_keys
                                                          .iter()
                                                          .map(|k| k.to_string())
                                                          .collect::<Vec<_>>()
                                                          .join(", ")
                                    ));
                                }
                                sidecar_path.insert(chan.iid, out.instrument_rel);
                                usable_sf2.push(chan);
                            }
                            Err(e) => skipped.push((chan, format!("Slicex: {e}"))),
                        }
                        continue;
                    }
                }

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

                if debug_on() {
                    if let Some(p) = chan.plugin.as_ref() {
                        eprintln!(
                            "flp: chan {:>3} native internal={:?} display={:?} vst_path={:?} state_len={}",
                            chan.iid,
                            p.internal_name,
                            p.display_name,
                            p.vst_path,
                            p.state.len(),
                        );
                    } else {
                        eprintln!("flp: chan {:>3} native, no plugin", chan.iid);
                    }
                }

                // 0. Slicex? (redundant if it was already caught above, but
                // some truly native channels might still be Slicex).
                if let Some(plugin) = chan.plugin.as_ref() {
                    if slicex::is_slicex(plugin) {
                        match slicex::convert(chan, plugin, placed, aux_files, &mut warnings, if prj.tempo > 0.0 { prj.tempo } else { super::DEFAULT_TEMPO }) {
                            Ok(out) => {
                                let (lo, hi) = (out.base_key, out.base_key + out.slices as u32);
                                let mut stray_keys: Vec<u32> =
                                placed.get(&chan.iid).map_or_else(Vec::new, |v| {
                                    v.iter()
                                    .map(|p| p.note.key as u32)
                                    .filter(|&k| k < lo || k >= hi)
                                    .collect()
                                });
                                let stray = stray_keys.len();
                                stray_keys.sort_unstable();
                                stray_keys.dedup();
                                if stray > 0 {
                                    warnings.push(format!(
                                        "[{:2}] Slicex: {} note(s) outside slice keys {}..{} will be silent (keys: {})",
                                                          chan.iid,
                                                          stray,
                                                          lo,
                                                          hi - 1,
                                                          stray_keys
                                                          .iter()
                                                          .map(|k| k.to_string())
                                                          .collect::<Vec<_>>()
                                                          .join(", ")
                                    ));
                                }
                                sidecar_path.insert(chan.iid, out.instrument_rel);
                                usable_sf2.push(chan);
                            }
                            Err(e) => skipped.push((chan, format!("Slicex: {e}"))),
                        }
                        continue;
                    }
                }

                // 1. SoundFont player?
                if let Some(plugin) = chan.plugin.as_ref() {
                    let is_sf2_plugin = sf2::is_sf2_player(plugin);
                    let has_sf2_marker = plugin
                    .state
                    .windows(4)
                    .any(|w| w.eq_ignore_ascii_case(b".sf2"));

                    if debug_on() {
                        let n = plugin.state.len();
                        eprintln!(
                            "flp: chan {} native internal={:?} sf2_player={} has_marker={} state={} bytes",
                            chan.iid, plugin.internal_name, is_sf2_plugin, has_sf2_marker, n,
                        );
                        if !has_sf2_marker && plugin.vst_path.is_empty() && n > 0 {
                            eprintln!(
                                "flp: chan {} first 64: {}",
                                chan.iid,
                                hex(&plugin.state[..n.min(64)])
                            );
                        }
                    }

                    if is_sf2_plugin
                        && plugin.vst_path.is_empty()
                        && plugin.state.len() < SF2_STATE_MAX
                        && has_sf2_marker
                        {
                            if let Some(sf) = sf2::parse_sf2_state(&plugin.state) {
                                match handle_sf2(
                                    chan,
                                    &sf,
                                    opts,
                                    flp_dir,
                                    &mut sf2_copied,
                                    &mut sf2_preset_cache,
                                    aux_files,
                                    &mut warnings,
                                ) {
                                    Sf2Outcome::Done(rel) => {
                                        sidecar_path.insert(chan.iid, rel);
                                        usable_sf2.push(chan);
                                        continue;
                                    }
                                    Sf2Outcome::PresetMissing(reason) => {
                                        warnings.push(format!("[{:2}] {}", chan.iid, reason));
                                    }
                                    Sf2Outcome::Skip(reason) => {
                                        skipped.push((chan, reason));
                                        continue;
                                    }
                                }
                            }
                        }
                }

                // 2. Embedded audio in plugin state?
                if let Some(plugin) = chan.plugin.as_ref() {
                    if let Some(emb) = extract_embedded(chan, &plugin.state, &mut warnings) {
                        usable_embedded.push((chan, emb));
                        continue;
                    }
                }

                // 3. VST
                let has_state = chan.plugin.as_ref().map_or(false, |p| !p.state.is_empty());
                if !has_state {
                    skipped.push((chan, "native channel without saved state".into()));
                    continue;
                }
                if debug_on() {
                    let p = chan.plugin.as_ref().unwrap();
                    eprintln!(
                        "flp: chan {} native -> VST branch: display={:?} internal={:?} vst_path={:?} state_len={}",
                        chan.iid,
                        p.display_name,
                        p.internal_name,
                        p.vst_path,
                        p.state.len()
                    );
                }
                let plugin = chan.plugin.as_ref().unwrap();
                let resolved = resolve_plugin(plugin, plugin_index, fallback_table);

                match &resolved {
                    Resolved::Exact { name, path } => {
                        warnings.push(format!(
                            "[{:2}] {}: using installed plugin at {}",
                            chan.iid,
                            name,
                            path.display()
                        ));
                    }
                    Resolved::Fallback {
                        name,
                        path,
                        replaces,
                        ..
                    } => {
                        let path_str = path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<not found>".into());
                        warnings.push(format!(
                            "[{:2}] {}: not installed, falling back to {} ({})",
                                              chan.iid, replaces, name, path_str
                        ));
                    }
                    Resolved::Unresolved { name } => {
                        warnings.push(format!(
                            "[{:2}] {}: not installed and no fallback configured — channel will be silent",
                            chan.iid, name
                        ));
                    }
                }

                resolved_map.insert(chan.iid, resolved);
                usable_natives.push(chan);

                if debug_on() {
                    eprintln!(
                        "flp: chan {:>3} native -> {:?}",
                        chan.iid,
                        match resolved_map.get(&chan.iid) {
                            Some(Resolved::Exact { name, .. }) => format!("Exact({})", name),
                              Some(Resolved::Fallback { name, path, .. }) =>
                              format!("Fallback({} -> {:?})", name, path),
                                  Some(Resolved::Unresolved { name }) => format!("Unresolved({})", name),
                              None => "??".into(),
                        }
                    );
                }
            }
            other => {
                skipped.push((chan, format!("channel type '{}' not converted", other)));
            }
        }
    }

    // ── VST sidecars for every usable native ─────────────────────────
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
            let resolved = resolved_map
            .get(&chan.iid)
            .expect("every usable native has a resolution");
            match vst_sidecar(&sidecar_name, chan, resolved) {
                Ok(files) => {
                    aux_files.extend(files);
                    sidecar_path.insert(
                        chan.iid,
                        format!("instruments/{}.instrument", sidecar_name),
                    );
                }
                Err(e) => {
                    warnings.push(format!(
                        "[{:2}] sidecar write failed: {:#}",
                        chan.iid, e
                    ));
                }
            }
        }
    }

    Classified {
        usable_samples,
        usable_embedded,
        usable_natives,
        usable_sf2,
        resolved_map,
        sidecar_path,
        skipped,
        warnings,
        envelope_only,
    }
}

// ── SF2 handling ───────────────────────────────────────────────────────

fn handle_sf2(
    chan: &Channel,
    sf: &SfState,
    opts: &ConvertOptions,
    flp_dir: &Path,
    sf2_copied: &mut HashMap<PathBuf, String>,
    preset_cache: &mut HashMap<PathBuf, Option<Vec<(u16, u16)>>>,
              aux_files: &mut Vec<AuxFile>,
              warnings: &mut Vec<String>,
) -> Sf2Outcome {
    let want = sf
    .path
    .rsplit(|c| c == '\\' || c == '/')
    .next()
    .unwrap_or("?")
    .to_string();

    let soundfonts_dir: Option<PathBuf> =
    Some(crate::paths::system_data().join("soundfonts"))
    .filter(|p| p.is_dir());

    let mut dirs: Vec<&Path> = Vec::with_capacity(3);
    if let Some(d) = opts.samples_dir.as_deref() {
        dirs.push(d);
    }
    dirs.push(flp_dir);
    if let Some(d) = soundfonts_dir.as_deref() {
        dirs.push(d);
    }

    if debug_on() {
        eprintln!(
            "flp: chan {} sf2 state: path={:?} bank={} patch={} dirs={}",
            chan.iid,
            want,
            sf.bank,
            sf.preset,
            dirs.len()
        );
    }

    let Some(src) = sf2::find_sf2(Some(sf), &chan.name, &dirs) else {
        return Sf2Outcome::Skip(format!("SF2 not found: {}", want));
    };
    if debug_on() {
        eprintln!(
            "flp: chan {} sf2 {} bank {} patch {}",
            chan.iid,
            src.display(),
                  sf.bank,
                  sf.preset
        );
    }

    match sf2_presets_of(&src, preset_cache) {
        Some(presets) => {
            let bank_u16 = sf.bank as u16;
            let preset_u16 = sf.preset as u16;
            if !presets.iter().any(|(b, p)| *b == bank_u16 && *p == preset_u16) {
                return Sf2Outcome::PresetMissing(format!(
                    "{}: SF2 preset ({}, {}) not in {} — routing to fallback",
                                                         if chan.name.is_empty() { &chan.channel_type } else { &chan.name },
                                                             sf.bank,
                                                         sf.preset,
                                                         src.display(),
                ));
            }
        }
        None => {}
    }

    let base_raw = if chan.name.is_empty() {
        format!("Ch{:02}", chan.iid)
    } else {
        chan.name.clone()
    };
    let base = identifier(&base_raw);

    match write_sf2_sidecar(
        chan,
        &base,
        &src,
        sf2_copied,
        aux_files,
        warnings,
        sf.bank,
        sf.preset,
    ) {
        Some(rel) => Sf2Outcome::Done(rel),
        None => Sf2Outcome::Skip(format!(
            "SF2 sidecar write failed for {}",
            src.display()
        )),
    }
}

fn sf2_presets_of(
    path: &Path,
    cache: &mut HashMap<PathBuf, Option<Vec<(u16, u16)>>>,
) -> Option<Vec<(u16, u16)>> {
    if let Some(cached) = cache.get(path) {
        return cached.clone();
    }
    let result = (|| {
        let mut engine = crate::hosting::sf2::engine::Sf2Engine::new(44100);
        engine.load(path).ok()?;
        Some(
            engine
            .list_presets()
            .into_iter()
            .map(|(b, p, _)| (b, p))
            .collect(),
        )
    })();
    cache.insert(path.to_path_buf(), result.clone());
    result
}

fn write_sf2_sidecar(
    chan: &Channel,
    base: &str,
    src: &Path,
    sf2_copied: &mut HashMap<PathBuf, String>,
    aux_files: &mut Vec<AuxFile>,
    warnings: &mut Vec<String>,
    bank: u32,
    preset: u32,
) -> Option<String> {
    let rel: Option<String> = match sf2_copied.get(src) {
        Some(r) => Some(r.clone()),
        None => match std::fs::read(src) {
            Ok(bytes) => {
                let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("sf2");
                let mut rel = format!("samples/{}.sf2", safe_name(stem));
                if sf2_copied.values().any(|v| *v == rel) {
                    rel = format!("samples/{:02}_{}.sf2", chan.iid, safe_name(stem));
                }
                aux_files.push(AuxFile {
                    rel_path: rel.clone(),
                               bytes,
                });
                sf2_copied.insert(src.to_path_buf(), rel.clone());
                Some(rel)
            }
            Err(e) => {
                warnings.push(format!(
                    "[{:2}] SF2 {} read failed: {:#}",
                    chan.iid,
                    src.display(),
                                      e
                ));
                None
            }
        },
    };
    let rel = rel?;

    let inst_text = format!(
        "Instrument {base}\n{{\n    Kind: sf2;\n    File: '{rel}';\n    Bank: {};\n    Preset: {};\n    SourceChannel: {};\n}}\n",
        bank, preset, chan.iid
    );
    let inst_rel = format!("instruments/{}.instrument", base);
    aux_files.push(AuxFile {
        rel_path: inst_rel.clone(),
                   bytes: inst_text.into_bytes(),
    });
    warnings.push(format!(
        "[{:2}] {}: SF2 {} bank {} patch {}",
        chan.iid,
        chan.name,
        src.display(),
                          bank,
                          preset,
    ));
    Some(inst_rel)
}

// ── embedded audio handling ────────────────────────────────────────────

fn extract_embedded(
    chan: &Channel,
    state: &[u8],
    warnings: &mut Vec<String>,
) -> Option<EmbeddedSample> {
    let found = flp_audio::extract_all(state);
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

    let mut all_zones: Vec<(Vec<u8>, Option<u8>)> = Vec::new();
    for (i, aud) in found.iter().enumerate() {
        if aud.data.len() < MIN_EMBEDDED_BYTES {
            if debug_on() {
                eprintln!(
                    "flp: chan {} embedded #{} skipped ({} bytes < {} byte floor)",
                          chan.iid,
                          i,
                          aud.data.len(),
                          MIN_EMBEDDED_BYTES
                );
            }
            continue;
        }
        let wav = if aud.extension == "flac" {
            match flp_audio::flac_to_wav(&aud.data) {
                Ok(w) => w,
                Err(e) => {
                    warnings.push(format!(
                        "[{:2}] embedded FLAC #{} failed to decode: {:#}",
                        chan.iid, i, e
                    ));
                    continue;
                }
            }
        } else {
            aud.data.clone()
        };
        let root_key = flp_audio::wav_root_key(&wav);
        all_zones.push((wav, root_key));
    }

    let files_found = all_zones.len();
    let (wav0, root0) = all_zones.first().cloned()?;
    if files_found >= 2 {
        warnings.push(format!(
            "[{:2}] {} zone(s) found; all written to samples.json and {}",
                              chan.iid, files_found, FIDELITY_FILENAME
        ));
    }
    Some(EmbeddedSample {
        wav: wav0,
         root_key: root0,
         files_found,
         raw_len: found[0].data.len(),
         all_zones,
    })
}
