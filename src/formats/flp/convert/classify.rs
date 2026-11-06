//! Classify each FLP channel into one of five buckets: external sample,
//! embedded sample, SF2 (native player or Slicex-generated), VST, or
//! nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::deps::index::PluginIndex;
use crate::deps::registry::RegistryTable;
use crate::formats::flp::fidelity::FIDELITY_FILENAME;
use crate::formats::flp::model::{self as flp_src, Channel};
use crate::formats::flp::audio as flp_audio;
use crate::conversion::{identifier, safe_name, AuxFile, ConvertOptions};

use super::place::Placed;
use super::render::EmbeddedSample;
use super::resolve::{resolve_plugin, vst_sidecar, Resolved};
use super::sf2::{self, SfState};
use crate::formats::flp::slicex::convert as slicex;
use super::wav::find_sample;
use super::{debug_on, SF2_STATE_MAX};
use crate::formats::flp::MIN_EMBEDDED_BYTES;

// ── output struct ─────────────────────────────────────────────────────

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
    pub(super) usable_slicex_halotext: Vec<&'a Channel>,
    pub(super) slicex_halotext: HashMap<u16, slicex::HalotextOut>,
}

// ── dispatch context ──────────────────────────────────────────────────

struct Ctx<'a> {
    placed: &'a HashMap<u16, Vec<Placed>>,
    opts: &'a ConvertOptions,
    flp_dir: &'a Path,
    plugin_index: &'a PluginIndex,
    fallback_table: Option<&'a RegistryTable>,
    tempo: f64,
}

#[derive(Default)]
struct Buckets<'a> {
    usable_samples: Vec<(&'a Channel, PathBuf)>,
    usable_embedded: Vec<(&'a Channel, EmbeddedSample)>,
    usable_natives: Vec<&'a Channel>,
    usable_sf2: Vec<&'a Channel>,
    resolved_map: HashMap<u16, Resolved>,
    sidecar_path: HashMap<u16, String>,
    skipped: Vec<(&'a Channel, String)>,
    warnings: Vec<String>,
    envelope_only: Vec<(u16, String, usize)>,
    // ← These two were missing.
    usable_slicex_halotext: Vec<&'a Channel>,
    slicex_halotext: HashMap<u16, slicex::HalotextOut>,
}

impl<'a> Buckets<'a> {
    fn merge(&mut self, other: Buckets<'a>) {
        self.usable_samples.extend(other.usable_samples);
        self.usable_embedded.extend(other.usable_embedded);
        self.usable_natives.extend(other.usable_natives);
        self.usable_sf2.extend(other.usable_sf2);
        self.resolved_map.extend(other.resolved_map);
        self.sidecar_path.extend(other.sidecar_path);
        self.skipped.extend(other.skipped);
        self.warnings.extend(other.warnings);
        self.envelope_only.extend(other.envelope_only);
        self.usable_slicex_halotext.extend(other.usable_slicex_halotext);
        self.slicex_halotext.extend(other.slicex_halotext);
    }
}

#[derive(Default)]
struct Sf2Cache {
    copied: HashMap<PathBuf, String>,
    preset_cache: HashMap<PathBuf, Option<Vec<(u16, u16)>>>,
}

enum HandlerOutcome {
    Done,
    Skipped(String),
}

type HandlerResult = Option<HandlerOutcome>;

type ChannelResult<'a> = (HandlerOutcome, Buckets<'a>, Vec<AuxFile>);

// ── entry point ───────────────────────────────────────────────────────

pub(super) fn classify_channels<'a>(
    prj: &'a flp_src::Project,
    placed: &'a HashMap<u16, Vec<Placed>>,
    opts: &'a ConvertOptions,
    flp_dir: &'a Path,
    plugin_index: &'a PluginIndex,
    fallback_table: Option<&'a RegistryTable>,
    aux_files: &mut Vec<AuxFile>,
) -> Classified<'a> {
    let tempo = if prj.tempo > 0.0 { prj.tempo } else { super::DEFAULT_TEMPO };
    let cx = Ctx { placed, opts, flp_dir, plugin_index, fallback_table, tempo };
    let n_channels = prj.channels.len();

    let per_channel: Vec<ChannelResult<'a>> = prj
    .channels
    .par_iter()
    .enumerate()
    .map(|(idx, chan)| classify_one(idx, n_channels, chan, &cx))
    .collect();

    let mut merged = Buckets::default();
    let mut merged_aux: Vec<AuxFile> = Vec::new();
    for (idx, (outcome, local, local_aux)) in per_channel.into_iter().enumerate() {
        merged.merge(local);
        merged_aux.extend(local_aux);
        if let HandlerOutcome::Skipped(reason) = outcome {
            merged.skipped.push((&prj.channels[idx], reason));
        }
    }

    write_all_vst_sidecars(&mut merged, &mut merged_aux);
    aux_files.extend(merged_aux);

    Classified {
        usable_samples: merged.usable_samples,
        usable_embedded: merged.usable_embedded,
        usable_natives: merged.usable_natives,
        usable_sf2: merged.usable_sf2,
        resolved_map: merged.resolved_map,
        sidecar_path: merged.sidecar_path,
        skipped: merged.skipped,
        warnings: merged.warnings,
        envelope_only: merged.envelope_only,
        usable_slicex_halotext: merged.usable_slicex_halotext,
        slicex_halotext: merged.slicex_halotext,
    }
}

fn classify_one<'a>(
    idx: usize,
    n_total: usize,
    chan: &'a Channel,
    cx: &Ctx<'a>,
) -> ChannelResult<'a> {
    let t_chan = std::time::Instant::now();
    let chan_label: String = if chan.name.is_empty() {
        chan.channel_type.clone()
    } else { chan.name.clone() };

    crate::log_event!(
        "pipe.progress",
        stage: "classify",
        current: format!("{}", idx + 1),
                      total: format!("{}", n_total),
                      detail: format!("chan {} {} starting", chan.iid, chan_label)
    );

    let mut out: Buckets<'a> = Buckets::default();
    let mut aux_files: Vec<AuxFile> = Vec::new();
    let mut sf2_cache = Sf2Cache::default();

    let outcome = classify_one_inner(chan, cx, &mut out, &mut sf2_cache, &mut aux_files);

    let elapsed = t_chan.elapsed().as_millis();
    crate::log_event!(
        "pipe.progress",
        stage: "classify",
        current: format!("{}", idx + 1),
                      total: format!("{}", n_total),
                      detail: format!("chan {} {} done in {} ms", chan.iid, chan_label, elapsed)
    );

    (outcome, out, aux_files)
}

fn classify_one_inner<'a>(
    chan: &'a Channel,
    cx: &Ctx<'a>,
    out: &mut Buckets<'a>,
    sf2_cache: &mut Sf2Cache,
    aux_files: &mut Vec<AuxFile>,
) -> HandlerOutcome {
    if !chan.enabled.unwrap_or(true) {
        return HandlerOutcome::Skipped("disabled in FL".into());
    }

    let active_envs = chan.envelopes.iter().filter(|e| e.env_enabled != 0).count();
    if active_envs > 0 {
        let label = if chan.name.is_empty() {
            chan.channel_type.clone()
        } else { chan.name.clone() };
        out.envelope_only.push((chan.iid, label, active_envs));
    }

    let has_notes = cx.placed.get(&chan.iid).map_or(false, |v| !v.is_empty());
    if !has_notes {
        let has_content = !chan.sample_path.is_empty()
        || chan.plugin.as_ref().map_or(false, |p| !p.state.is_empty());
        if has_content && matches!(chan.channel_type.as_str(), "sampler" | "instrument" | "native") {
            return HandlerOutcome::Skipped("no notes".into());
        }
        return HandlerOutcome::Done;
    }

    dispatch(chan, cx, out, sf2_cache, aux_files)
}

fn dispatch<'a>(
    chan: &'a Channel,
    cx: &Ctx<'a>,
    out: &mut Buckets<'a>,
    sf2_cache: &mut Sf2Cache,
    aux_files: &mut Vec<AuxFile>,
) -> HandlerOutcome {
    if chan.channel_type == "native" && cx.opts.skip_native {
        return HandlerOutcome::Skipped("native channel; --skip-native".into());
    }

    if let Some(o) = try_slicex(chan, cx, out, aux_files) { return o; }
    if let Some(o) = try_sf2_player(chan, cx, out, sf2_cache, aux_files) { return o; }
    if let Some(o) = try_sampler(chan, cx, out) { return o; }
    if let Some(o) = try_embedded(chan, cx, out) { return o; }
    if let Some(o) = try_vst(chan, cx, out) { return o; }

    HandlerOutcome::Skipped(format!("channel type '{}' not converted", chan.channel_type))
}

// ── handlers ─────────────────────────────────────────────────────────

fn try_slicex<'a>(
    chan: &'a Channel,
    cx: &Ctx<'a>,
    out: &mut Buckets<'a>,
    aux_files: &mut Vec<AuxFile>,
) -> HandlerResult {
    let plugin = chan.plugin.as_ref()?;
    match slicex::convert(chan, plugin, aux_files, &mut out.warnings, cx.tempo) {
        Ok(slicex::SlicexOut::Sf2(sf2_out)) => {
            out.sidecar_path.insert(chan.iid, sf2_out.instrument_rel);
            out.usable_sf2.push(chan);
            Some(HandlerOutcome::Done)
        }
        Ok(slicex::SlicexOut::Halotext(ht)) => {
            out.slicex_halotext.insert(chan.iid, ht);
            out.usable_slicex_halotext.push(chan);
            Some(HandlerOutcome::Done)
        }
        Err(e) => {
            if debug_on() {
                eprintln!("flp: chan {:>3} slicex: {} (not Slicex, trying next)", chan.iid, e);
            }
            None
        }
    }
}

fn try_sf2_player<'a>(
    chan: &'a Channel,
    cx: &Ctx<'a>,
    out: &mut Buckets<'a>,
    sf2_cache: &mut Sf2Cache,
    aux_files: &mut Vec<AuxFile>,
) -> HandlerResult {
    if chan.channel_type != "native" { return None; }
    let plugin = chan.plugin.as_ref()?;

    let is_sf2_plugin = sf2::is_sf2_player(plugin);
    let has_sf2_marker = plugin.state.windows(4).any(|w| w.eq_ignore_ascii_case(b".sf2"));

    if !is_sf2_plugin
        || !plugin.vst_path.is_empty()
        || plugin.state.len() >= SF2_STATE_MAX
        || !has_sf2_marker
        {
            return None;
        }

        let sf = sf2::parse_sf2_state(&plugin.state)?;

    match handle_sf2(
        chan, &sf, cx.opts, cx.flp_dir,
        &mut sf2_cache.copied, &mut sf2_cache.preset_cache,
        aux_files, &mut out.warnings,
    ) {
        Sf2Outcome::Done(rel) => {
            out.sidecar_path.insert(chan.iid, rel);
            out.usable_sf2.push(chan);
            Some(HandlerOutcome::Done)
        }
        Sf2Outcome::PresetMissing(reason) => {
            out.warnings.push(format!("[{:2}] {}", chan.iid, reason));
            None
        }
        Sf2Outcome::Skip(reason) => Some(HandlerOutcome::Skipped(reason)),
    }
}

fn try_sampler<'a>(chan: &'a Channel, cx: &Ctx<'a>, out: &mut Buckets<'a>) -> HandlerResult {
    if chan.channel_type != "sampler" && chan.channel_type != "instrument" {
        return None;
    }
    if chan.sample_path.is_empty() {
        return Some(HandlerOutcome::Skipped("no sample path".into()));
    }
    match find_sample(&chan.sample_path, cx.opts.samples_dir.as_deref(), Some(cx.flp_dir)) {
        Some(p) => {
            out.usable_samples.push((chan, p));
            Some(HandlerOutcome::Done)
        }
        None => {
            let basename = Path::new(&chan.sample_path.replace('\\', "/"))
            .file_name().and_then(|s| s.to_str()).unwrap_or("?").to_string();
            Some(HandlerOutcome::Skipped(format!("sample not found: {}", basename)))
        }
    }
}

fn try_embedded<'a>(chan: &'a Channel, _cx: &Ctx<'a>, out: &mut Buckets<'a>) -> HandlerResult {
    if chan.channel_type != "native" { return None; }
    let plugin = chan.plugin.as_ref()?;
    let emb = extract_embedded(chan, &plugin.state, &mut out.warnings)?;
    out.usable_embedded.push((chan, emb));
    Some(HandlerOutcome::Done)
}

fn try_vst<'a>(chan: &'a Channel, cx: &Ctx<'a>, out: &mut Buckets<'a>) -> HandlerResult {
    if chan.channel_type != "native" {
        return Some(HandlerOutcome::Skipped(format!(
            "channel type '{}' not converted", chan.channel_type)));
    }
    let Some(plugin) = chan.plugin.as_ref() else {
        return Some(HandlerOutcome::Skipped("native channel without saved state".into()));
    };
    if plugin.state.is_empty() {
        return Some(HandlerOutcome::Skipped("native channel without saved state".into()));
    }

    let resolved = resolve_plugin(plugin, cx.plugin_index, cx.fallback_table);
    match &resolved {
        Resolved::Exact { name, path } => {
            out.warnings.push(format!(
                "[{:2}] {}: using installed plugin at {}",
                chan.iid, name, path.display()));
        }
        Resolved::Fallback { name, path, replaces, .. } => {
            let path_str = path.as_ref().map(|p| p.display().to_string())
            .unwrap_or_else(|| "<not found>".into());
            out.warnings.push(format!(
                "[{:2}] {}: not installed, falling back to {} ({})",
                                      chan.iid, replaces, name, path_str));
        }
        Resolved::Unresolved { name } => {
            out.warnings.push(format!(
                "[{:2}] {}: not installed and no fallback configured — channel will be silent",
                chan.iid, name));
        }
    }
    out.resolved_map.insert(chan.iid, resolved);
    out.usable_natives.push(chan);
    Some(HandlerOutcome::Done)
}

// ── deferred sidecar writing ──────────────────────────────────────────

fn write_all_vst_sidecars(out: &mut Buckets, aux_files: &mut Vec<AuxFile>) {
    let chans: Vec<&Channel> = out.usable_natives.clone();
    let mut used: HashMap<String, u32> = HashMap::new();

    for chan in chans {
        let base_raw = if chan.name.is_empty() {
            format!("Ch{:02}", chan.iid)
        } else { chan.name.clone() };
        let base = identifier(&base_raw);
        let count_ref = used.entry(base.clone()).or_insert(0);
        *count_ref += 1;
        let count = *count_ref;
        let sidecar_name = if count == 1 { base } else { format!("{}{:02}", base, count) };

        let result: Result<Vec<AuxFile>, String> = {
            let Some(resolved) = out.resolved_map.get(&chan.iid) else { continue };
            vst_sidecar(&sidecar_name, chan, resolved)
            .map_err(|e| format!("[{:2}] sidecar write failed: {:#}", chan.iid, e))
        };

        match result {
            Ok(files) => {
                aux_files.extend(files);
                out.sidecar_path.insert(chan.iid, format!("instruments/{}.instrument", sidecar_name));
            }
            Err(msg) => out.warnings.push(msg),
        }
    }
}

// ── SF2 handling ──────────────────────────────────────────────────────

enum Sf2Outcome {
    Done(String),
    PresetMissing(String),
    Skip(String),
}

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
    let want = sf.path.rsplit(|c| c == '\\' || c == '/').next().unwrap_or("?").to_string();

    let sf2_roots: Vec<PathBuf> = crate::paths::soundfont_roots()
    .into_iter().filter(|p| p.is_dir()).collect();

    let mut dirs: Vec<&Path> = Vec::with_capacity(2 + sf2_roots.len());
    if let Some(d) = opts.samples_dir.as_deref() { dirs.push(d); }
    dirs.push(flp_dir);
    for root in &sf2_roots { dirs.push(root.as_path()); }

    let src: PathBuf = if let Some(p) = crate::paths::find_soundfont(&want) {
        p
    } else if let Some(p) = sf2::find_sf2(Some(sf), &chan.name, &dirs) {
        p
    } else {
        let roots: Vec<String> = crate::paths::soundfont_roots().iter().map(|p| {
            format!("  {}{}", p.display(), if p.is_dir() { "" } else { " (missing)" })
        }).collect();
        return Sf2Outcome::Skip(format!(
            "SF2 not found: {}\nsearched:\n{}\nset CIRCOLATE_SOUNDFONT_DIR to override",
            want, roots.join("\n")));
    };

    let mut resolved_bank = sf.bank;
    let mut resolved_preset = sf.preset;

    if let Some(presets) = sf2_presets_of(&src, preset_cache) {
        if let Some((fb_bank, fb_preset)) =
            sf2::resolve_sf2_preset(sf.bank as u16, sf.preset as u16, &presets)
            {
                resolved_bank = fb_bank as u32;
                resolved_preset = fb_preset as u32;
                if resolved_bank != sf.bank || resolved_preset != sf.preset {
                    warnings.push(format!(
                        "[{:2}] {}: SF2 preset ({}, {}) not found in {} — using nearest ({}, {})",
                                          chan.iid,
                                          if chan.name.is_empty() { &chan.channel_type } else { &chan.name },
                                              sf.bank, sf.preset, src.display(), resolved_bank, resolved_preset));
                }
            } else {
                return Sf2Outcome::PresetMissing(format!(
                    "{}: SF2 has no presets in {} — routing to fallback",
                    if chan.name.is_empty() { &chan.channel_type } else { &chan.name },
                        src.display()));
            }
    }

    let base_raw = if chan.name.is_empty() {
        format!("Ch{:02}", chan.iid)
    } else { chan.name.clone() };
    let base = identifier(&base_raw);

    match write_sf2_sidecar(
        chan, &base, &src, sf2_copied, aux_files, warnings, resolved_bank, resolved_preset,
    ) {
        Some(rel) => Sf2Outcome::Done(rel),
        None => Sf2Outcome::Skip(format!("SF2 sidecar write failed for {}", src.display())),
    }
}

fn sf2_presets_of(
    path: &Path,
    cache: &mut HashMap<PathBuf, Option<Vec<(u16, u16)>>>,
) -> Option<Vec<(u16, u16)>> {
    if let Some(cached) = cache.get(path) { return cached.clone(); }
    let result = sf2::read_sf2_presets(path);
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
                aux_files.push(AuxFile { rel_path: rel.clone(), bytes });
                sf2_copied.insert(src.to_path_buf(), rel.clone());
                Some(rel)
            }
            Err(e) => {
                warnings.push(format!(
                    "[{:2}] SF2 {} read failed: {:#}", chan.iid, src.display(), e));
                None
            }
        },
    };
    let rel = rel?;

    let inst_text = format!(
        "Instrument {base}\n{{\n    Kind: sf2;\n    File: '{rel}';\n    Bank: {};\n    Preset: {};\n    SourceChannel: {};\n}}\n",
        bank, preset, chan.iid);
    let inst_rel = format!("instruments/{}.instrument", base);
    aux_files.push(AuxFile { rel_path: inst_rel.clone(), bytes: inst_text.into_bytes() });
    warnings.push(format!(
        "[{:2}] {}: SF2 {} bank {} patch {}",
        chan.iid, chan.name, src.display(), bank, preset));
    Some(inst_rel)
}

// ── embedded audio handling ───────────────────────────────────────────

fn extract_embedded(
    chan: &Channel,
    state: &[u8],
    warnings: &mut Vec<String>,
) -> Option<EmbeddedSample> {
    let found = flp_audio::extract_all(state);

    let mut all_zones: Vec<(Vec<u8>, Option<u8>)> = Vec::new();
    for (i, aud) in found.iter().enumerate() {
        if aud.data.len() < MIN_EMBEDDED_BYTES { continue; }
        let wav = if aud.extension == "flac" {
            match flp_audio::flac_to_wav(&aud.data) {
                Ok(w) => w,
                Err(e) => {
                    warnings.push(format!(
                        "[{:2}] embedded FLAC #{} failed to decode: {:#}",
                        chan.iid, i, e));
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
                              chan.iid, files_found, FIDELITY_FILENAME));
    }
    Some(EmbeddedSample {
        wav: wav0,
         root_key: root0,
         files_found,
         raw_len: found[0].data.len(),
         all_zones,
    })
}
