//! Attaching instruments and effects to the audio `Engine`.
//!
//! Every `install_*` function operates on a live `player::Engine` and
//! attaches one kind of host: VST3 instruments, native SF2 engines,
//! master FX chain, or effect shims (fallback VST3s for effects the
//! halotext references but the user doesn't have).
//!
//! **Plugin resolution is by name only.**  Recorded `Path:` fields are
//! never consulted — see the module docs on `crate::paths` for the
//! reasoning.  The transitional bridge for old halotexts (which
//! still carry a `Path:` but no `plugin_name` field on
//! `VstInstrument`) is to derive the name from
//! `plugin_path.file_stem()`; the path itself is never printed or
//! loaded.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::deps::plugins::PluginTable;
use crate::effects::EffectChain;
use crate::hosting::vst::ffi::VstHost;
use crate::paths;
use crate::player::{self, Engine};

// ── VST3 instruments ──────────────────────────────────────────────────

/// Everything both playback paths need, in one place.
pub struct InstallOptions<'a> {
    pub no_master_fx: bool,
    pub sf2_override: Option<&'a Path>,
    /// Multiplier applied to fallback VSTs.  Live playback uses 1.0;
    /// offline rendering honours `VST_FALLBACK_GAIN`.
    pub fallback_gain: f32,
    pub share_dir: Option<&'a Path>,
}

/// One installer's outcome.  `loaded` counts *instantiated* hosts/engines,
/// never map entries or aliases.  `failures` carries the human-readable
/// reason so callers can surface it without re-reading the log.
#[derive(Debug, Clone, Default)]
pub struct InstallCounts {
    pub loaded: usize,
    pub failures: Vec<String>,
}

impl InstallCounts {
    fn fail(&mut self, reason: impl Into<String>) {
        self.failures.push(reason.into());
    }
}

/// Typed summary of one `install_all` call.  Playback and offline render
/// see exactly the same numbers; "loaded" no longer means "referenced".
#[derive(Debug, Clone, Default)]
pub struct InstallReport {
    pub vst: InstallCounts,
    pub sf2: InstallCounts,
    pub shims: InstallCounts,
    pub chip_players: usize,
    pub master_fx: usize,
}

/// The single engine setup entry point.
pub fn install_all(
    engine: &mut Engine,
    project: &player::Project,
    opts: &InstallOptions<'_>,
) -> InstallReport {
    let mut report = InstallReport {
        vst: install_vst_hosts(engine, project, opts.fallback_gain),
        sf2: install_sf2_engines(engine, project, opts.sf2_override),
        shims: install_shims(engine, project, opts.share_dir),
        ..Default::default()
    };
    crate::player::inserts::install_chains(engine, project, project.sample_rate);
    report.chip_players = install_chip_players(engine, project, opts.share_dir);
    report.master_fx = install_master_fx(engine, project, opts.no_master_fx);

    let failures = report.vst.failures.len()
        + report.sf2.failures.len()
        + report.shims.failures.len();
    crate::log_event!(
        "player.install.summary",
        vst: report.vst.loaded,
        sf2: report.sf2.loaded,
        shims: report.shims.loaded,
        chip: report.chip_players,
        fx: report.master_fx,
        failures: failures,
    );
    for (kind, reasons) in [
        ("vst", &report.vst.failures),
        ("sf2", &report.sf2.failures),
        ("shim", &report.shims.failures),
    ] {
        // A missing plugin often appears on every shim event; report it
        // once with a count instead of flooding the log overlay.
        let mut seen: Vec<(&str, usize)> = Vec::new();
        for reason in reasons {
            let text = reason.as_str();
            if let Some(entry) = seen.iter_mut().find(|(known, _)| *known == text) {
                entry.1 += 1;
            } else {
                seen.push((text, 1));
            }
        }
        for (reason, count) in seen {
            crate::log_event!(
                "player.install.failed",
                kind: kind,
                err: reason,
                count: count,
            );
        }
    }
    report
}

/// The one mixer key each VST iid should use.  `vst_instruments` contains
/// aliases (`Sawer.vst3`, `Surge XT.vst3`, the sidecar path) that all point
/// at the same host.  Registering every alias made the last unordered map
/// entry win; the canonical key is the part's `sample_path`, which is what
/// the TUI and note scheduler use.
fn vst_mixer_paths(project: &player::Project) -> HashMap<u16, String> {
    let mut out = HashMap::new();
    for part in project.parts.values() {
        if let Some(inst) = project.vst_instruments.get(player::mixer_key(part)) {
            out.entry(inst.iid).or_insert_with(|| player::mixer_key(part).to_owned());
        }
    }
    out
}

pub fn install_vst_hosts(
    engine: &mut Engine,
    project: &player::Project,
    fallback_gain: f32,
) -> InstallCounts {
    let mut counts = InstallCounts::default();
    if project.vst_instruments.is_empty() {
        return counts;
    }

    let index = PluginTable::get();
    let mixer_paths = vst_mixer_paths(project);
    // One host per iid.  Aliases share the same bundle and state; loading
    // each alias wasted seconds and made "hosts installed" meaningless.
    let mut installed: HashSet<u16> = HashSet::new();

    for (name, vinst) in &project.vst_instruments {
        if !installed.insert(vinst.iid) {
            continue;
        }

        let stem = vinst
        .plugin_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

        let Some(path) = vinst.locate(&index) else {
            installed.remove(&vinst.iid);
            counts.fail(format!("{}: not found (stem {:?})", name, stem));
            crate::log_event!(
                "player.install.vst_not_found",
                name: name.clone(),
                              stem: format!("{:?}", stem),
            );
            continue;
        };

        let eff_gain = if vinst.fallback {
            vinst.gain * fallback_gain
        } else {
            vinst.gain
        };

        match VstHost::load(path.to_str().unwrap_or(""), &vinst.state_bytes, project.sample_rate) {
            Ok(host) => {
                let mixer_path = mixer_paths
                .get(&vinst.iid)
                .cloned()
                .unwrap_or_else(|| name.clone());
                engine.install_host_with_gain(vinst.iid, host, eff_gain);
                engine.register_part_path(vinst.iid, mixer_path.clone());
                counts.loaded += 1;
                crate::log_event!(
                    "player.install.vst_loaded",
                    name: vinst.name.clone(),
                                  plugin: path.display().to_string(),
                                  part: mixer_path,
                                  iid: vinst.iid.to_string(),
                );
            }
            Err(e) => {
                installed.remove(&vinst.iid);
                counts.fail(format!("{}: {:#}", name, e));
                crate::log_event!(
                    "vst.load_failed",
                    name: name.clone(),
                                  err: format!("{:#}", e),
                );
            }
        }
    }
    counts
}

// ── native SF2 engines ────────────────────────────────────────────────

/// The canonical mixer key for each SF2 iid.  `sf2_engine_specs` contains
/// duplicate aliases for the same preset (`samples/X.sf2#6` and
/// `X.sf2#6`) sharing one iid.  Registering each alias in HashMap order
/// made `part_volume` look up a key the TUI never writes.  Resolve the
/// iid through the actual parts instead.
fn sf2_mixer_paths(project: &player::Project) -> HashMap<u16, String> {
    let mut out = HashMap::new();
    for part in project.parts.values() {
        let mut keys = vec![part.sample_path.clone(), part.sf2_key()];
        if let Some(base) = std::path::Path::new(&part.sample_path)
            .file_name()
            .and_then(|s| s.to_str())
        {
            let p = part.preset.unwrap_or(0);
            let b = part.bank.unwrap_or(0);
            keys.push(if b == 0 {
                format!("{}#{}", base, p)
            } else {
                format!("{}#{}:{}", base, b, p)
            });
        }
        let Some(spec) = keys
            .iter()
            .find_map(|k| project.sf2_engine_specs.get(k))
        else { continue };
        out.entry(spec.iid).or_insert_with(|| player::mixer_key(part).to_owned());
    }
    out
}

pub fn install_sf2_engines(
    engine: &mut Engine,
    project: &player::Project,
    sf2_override: Option<&Path>,
) -> InstallCounts {
    let mut counts = InstallCounts::default();
    let sf2_debug = std::env::var_os("FLP_DEBUG").is_some();
    let default_gain = crate::hosting::sf2::engine::DEFAULT_GAIN;
    let mixer_paths = sf2_mixer_paths(project);
    // Aliases (`X.sf2#6` and `samples/X.sf2#6`) share one iid and one
    // engine.  Load it once; the canonical mixer key comes from the part.
    let mut installed: HashSet<u16> = HashSet::new();

    for (name, spec) in &project.sf2_engine_specs {
        if !installed.insert(spec.iid) {
            continue;
        }

        let path = sf2_override.unwrap_or(&spec.sf2_path);

        let mut sf2 = crate::hosting::sf2::engine::Sf2Engine::new(project.sample_rate);
        if let Err(e) = sf2.load(&path) {
            installed.remove(&spec.iid);
            counts.fail(format!("{}: {:#}", name, e));
            crate::log_event!(
                "player.install.sf2_load_failed",
                name: name.clone(),
                              path: path.display(),
                              err: format!("{:#}", e),
            );
            continue;
        }
        sf2.set_preset(spec.bank, spec.preset);
        let effective = default_gain * spec.gain;
        if sf2_debug {
            crate::log_event!(
                "player.install.sf2_debug",
                body: format!(
                    "  sf2 [iid={:>3}] {:<44} DEFAULT_GAIN={:.3} spec.gain={:.3} effective={:.3}",
                    spec.iid, name, default_gain, spec.gain, effective,
                ),
            );
        }
        sf2.set_gain(effective);
        engine.install_sf2_engine(spec.iid, sf2);
        counts.loaded += 1;

        // The engine's continuous mix pass looks up `part_volume` by iid.
        // Register the canonical part path, never the spec alias: the TUI
        // writes faders under `part.sample_path`.
        if let Some(part_path) = mixer_paths.get(&spec.iid) {
            engine.register_part_path(spec.iid, part_path.clone());
        }
    }
    crate::log_event!(
        "player.install.sf2_engines_installed",
        count: engine.sf2_engine_count(),
    );
    counts
}

// ── master FX ─────────────────────────────────────────────────────────

pub fn install_master_fx(engine: &mut Engine, project: &player::Project, disabled: bool) -> usize {
    if disabled {
        return 0;
    }
    match EffectChain::load_default(project.sample_rate) {
        Ok(chain) => {
            if chain.is_empty() {
                0
            } else {
                let len = chain.len();
                crate::log_event!("player.master_fx.loaded", name: chain.describe());
                engine.install_master_fx(chain);
                len
            }
        }
        Err(e) => {
            crate::log_event!(
                "player.master_fx.chain_failed",
                err: format!("{:#}", e),
            );
            0
        }
    }
}

// ── share dir helper ──────────────────────────────────────────────────

pub fn find_share_dir() -> Option<PathBuf> {
    // Use the same layered lookup as the registry: a fallbacks.json in the
    // user root wins, then localshare, then shipped system data.  Its
    // parent is the share tree that also holds `chip/` and `icons/`.
    paths::layered_file("fallbacks.json")
    .and_then(|p| p.parent().map(Path::to_path_buf))
}

// ── shims ─────────────────────────────────────────────────────────────

fn normalize_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    out
}

/// Resolve an effect-shim target name to a concrete plugin path.
fn resolve_effect_path(target: &str, share_dir: &Path) -> Option<PathBuf> {
    let target_norm = normalize_slug(target);

    let fallback_path = share_dir.join("fallbacks.json");
    let fallback_text = std::fs::read_to_string(&fallback_path).ok()?;
    let fallback_json: serde_json::Value = serde_json::from_str(&fallback_text).ok()?;
    let effects = fallback_json.get("effects").and_then(|v| v.as_array())?;

    let matched = effects.iter().find(|e| {
        e.get("match")
        .and_then(|v| v.as_str())
        .map(|m| normalize_slug(m) == target_norm)
        .unwrap_or(false)
    })?;

    let display_name = matched
    .get("fallback")
    .and_then(|f| f.get("name"))
    .and_then(|v| v.as_str())?;

    crate::log_event!(
        "player.install.shims_debug",
        body: format!("shim-debug: {:?} → {:?}", target_norm, display_name),
    );

    let resolved = PluginTable::get().find_any(display_name).cloned();
    if resolved.is_none() {
        crate::log_event!(
            "player.install.shims_debug",
            body: format!(
                "shim-debug: fallback {:?} not installed on this machine",
                display_name,
            ),
        );
    }
    resolved
}

pub fn install_shims(
    engine: &mut Engine,
    project: &player::Project,
    share_dir: Option<&Path>,
) -> InstallCounts {
    let mut counts = InstallCounts::default();
    if project.shims.is_empty() {
        return counts;
    }
    let Some(share_dir) = share_dir else {
        for shim in &project.shims {
            counts.fail(format!("{}: no share dir", shim.target));
        }
        crate::log_event!(
            "player.install.shims_no_share",
            count: project.shims.len(),
        );
        return counts;
    };
    crate::log_event!(
        "player.install.shims_debug",
        body: format!(
            "shim-debug: share_dir={:?} ({} shim(s) to attach)",
                      share_dir, project.shims.len(),
        ),
    );

    type ShimKey = (String, Vec<(u32, u64)>, u64, u64, u64);
    let mut hosts_by_key: HashMap<ShimKey, u32> = HashMap::new();
    let mut resolved_paths: HashMap<String, Option<PathBuf>> = HashMap::new();
    let mut next_shim_id: u32 = 0;
    let total_shims = project.shims.len();
    let mut attached = 0u32;
    let mut skipped = 0u32;

    for shim in &project.shims {
        let mut params_key: Vec<(u32, u64)> = shim
        .params
        .iter()
        .map(|p| (p.slot, p.value.to_bits()))
        .collect();
        params_key.sort();

        let key: ShimKey = (
            shim.target.clone(),
                            params_key,
                            shim.mix.to_bits(),
                            shim.gain.to_bits(),
                            shim.pan.to_bits(),
        );

        if let Some(&existing_id) = hosts_by_key.get(&key) {
            engine.attach_shim_line(shim.line as u32, existing_id);
            attached += 1;
            continue;
        }

        let path_opt = resolved_paths
        .entry(shim.target.clone())
        .or_insert_with(|| resolve_effect_path(&shim.target, share_dir));

        let Some(path) = path_opt.as_ref() else {
            counts.fail(format!("{}: not installed", shim.target));
            skipped += 1;
            continue;
        };

        let mut host = match VstHost::load(path.to_str().unwrap_or(""), &[], project.sample_rate) {
            Ok(h) => h,
            Err(e) => {
                counts.fail(format!("{}: {:#}", shim.target, e));
                crate::log_event!(
                    "vst.shim.load_failed",
                    target: shim.target.clone(),
                                  err: format!("{:#}", e),
                );
                skipped += 1;
                continue;
            }
        };

        for param in &shim.params {
            if param.slot == 0 {
                continue;
            }
            let index = (param.slot - 1) as u32;
            let normalized = (param.value / 100.0).clamp(0.0, 1.0);
            let ok = host.set_param_by_index(index, normalized);
            crate::log_event!(
                "player.install.shims_debug",
                body: format!(
                    "shim-debug: set_param({} idx={} val={:.3}) = {}",
                              shim.target, index, normalized, ok,
                ),
            );
        }

        let id = next_shim_id;
        next_shim_id += 1;
        engine.install_shim(
            id,
            host,
            shim.mix,
            (shim.gain / 100.0).clamp(0.0, 4.0),
                            shim.pan,
        );
        engine.attach_shim_line(shim.line as u32, id);
        hosts_by_key.insert(key, id);
        attached += 1;
        crate::log_event!(
            "player.install.shim_loaded",
            id: format!("{:02}", id),
                          target: shim.target.clone(),
        );
    }

    counts.loaded = attached as usize;
    crate::log_event!(
        "player.install.shims_summary",
        attached: attached,
        skipped: skipped,
        unique: next_shim_id,
        total: total_shims,
    );
    counts
}

/// Patch groups for the two chip players: `(PSG parts, FM parts)`.
///
/// Keys are `part.sample_path`.  This is the canonical mixer key: the
/// loader puts it in `ChipNote.part_path`, the TUI writes volume/solo/mute
/// under it, and `ChipPlayer::note_on` uses it for bank lookup.  Keying
/// banks by the halotext call name made notes play but silently broke
/// every per-part fader and mute toggle.
pub(crate) fn chip_patch_groups(
    project: &crate::player::project::Project,
) -> (Vec<(String, crate::formats::chip::patch::Patch)>, Vec<(String, crate::formats::chip::patch::Patch)>) {
    let mut psg_patches: Vec<(String, crate::formats::chip::patch::Patch)> = Vec::new();
    let mut fm_patches: Vec<(String, crate::formats::chip::patch::Patch)> = Vec::new();
    for part in project.parts.values() {
        let key = player::mixer_key(part);
        let Some(patch) = project.patch_instruments.get(key) else { continue };
        if patch.requires.iter().any(|r| r.capability == "sample.pcm") {
            continue;
        }
        if patch.requires.iter().any(|r| r.capability == "fm.operator") {
            fm_patches.push((key.to_owned(), patch.clone()));
        } else if patch.requires.iter().any(|r| {
            r.capability == "tone.square" || r.capability == "noise.lfsr"
        }) {
            psg_patches.push((key.to_owned(), patch.clone()));
        }
    }
    (psg_patches, fm_patches)
}

pub(crate) fn install_chip_players(
    engine: &mut crate::player::engine::Engine,
    project: &crate::player::project::Project,
    share_dir: Option<&std::path::Path>,
) -> usize {
    use crate::hosting::chip::{ChipPlayer, chip::parse as parse_chip};

    let mut installed = 0usize;
    let Some(share) = share_dir else { return installed };
    let mixer = std::sync::Arc::clone(&engine.mixer);

    let locate = |name: &str| -> Option<std::path::PathBuf> {
        let candidates = [
            share.join("chip").join(name),
            share.join("chip").join(format!("{}.txt", name)),
            share.parent().map(|p| p.join("chip").join(name)).unwrap_or_default(),
            share.parent().map(|p| p.join("chip").join(format!("{}.txt", name))).unwrap_or_default(),
            std::path::PathBuf::from("share/chip").join(name),
        ];
        candidates.into_iter().find(|c| !c.as_os_str().is_empty() && c.is_file())
    };

    let sn_chip = locate("sn76489.chip")
    .and_then(|p| std::fs::read_to_string(p).ok())
    .and_then(|t| parse_chip(&t).ok());

    let (psg_patches, fm_patches) = chip_patch_groups(project);

    // PSG player (SN76489 via CoreState)
    if !psg_patches.is_empty() {
        match sn_chip {
            Some(chip) => match ChipPlayer::with_mixer(
                chip,
                project.sample_rate,
                std::sync::Arc::clone(&mixer),
            ) {
                Ok(mut player) => {
                    let n = psg_patches.len();
                    if let Some(track) = project.vgm_dac.clone() { player.set_dac(track); }
                    for (path, patch) in psg_patches {
                        player.add_patch(path, patch);
                    }
                    engine.install_chip(100, "sn76489".into(), player);
                    installed += 1;
                    eprintln!("chip install: PSG player with {} patches", n);
                }
                Err(e) => eprintln!("PSG player init failed: {}", e),
            },
            None => eprintln!("sn76489.chip not found; PSG silent"),
        }
    }

    // FM player (YM2612 via YmfmBackend)
    if !fm_patches.is_empty() || project.vgm_dac.is_some() {
        match ChipPlayer::ymfm_with_mixer(
            7_670_453,
            project.sample_rate,
            std::sync::Arc::clone(&mixer),
        ) {
            Ok(mut player) => {
                let n = fm_patches.len();
                if let Some(track) = project.vgm_dac.clone() { player.set_dac(track); }
                for (path, patch) in fm_patches {
                    player.add_patch(path, patch);
                }
                engine.install_chip(101, "ym2612".into(), player);
                installed += 1;
                eprintln!("chip install: FM player with {} patches", n);
            }
            Err(e) => eprintln!("FM player init failed: {}", e),
        }
    }
    installed
}

#[cfg(test)]
mod tests {
    use super::{chip_patch_groups, sf2_mixer_paths, vst_mixer_paths};
    use crate::formats::chip::patch::{Envelope, Patch, PitchDecl, Requirement, SourceDecl};
    use crate::formats::halotext::model::HalotextPart;
    use crate::player::project::Project;
    use crate::player::types::{Sf2EngineSpec, VstInstrument};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn vgm_part(name: &str) -> HalotextPart {
        HalotextPart {
            name: name.to_string(),
            sample_path: format!("banks/song/{name}.patch"),
            beats_per_measure: 4,
            subs_per_beat: 4,
            len_beats: None,
            sample_offset: 0,
            preset: None,
            bank: None,
            src_bpm: None,
            events: Vec::new(),
            measures: Vec::new(),
            section_lengths: Vec::new(),
            default_vol: None,
            default_pan: None,
            default_dur: None,
            shims: Vec::new(),
            slicex: None,
        }
    }

    fn vgm_patch(cap: &str) -> Patch {
        Patch {
            name: String::new(),
            author: String::new(),
            license: String::new(),
            origin: String::new(),
            parent: None,
            requires: vec![Requirement { capability: cap.to_string(), constraints: Default::default() }],
            source: SourceDecl { kind: cap.to_string(), keys: Default::default() },
            pitch: PitchDecl::default(),
            amp: Envelope::None,
            filter: None,
            r#mod: None,
            native: Default::default(),
        }
    }

    #[test]
    fn chip_patch_groups_key_by_mixer_path() {
        let mut project = Project::empty(48000);
        project.parts.insert("FM1_aabbccdd".to_string(), vgm_part("FM1_aabbccdd"));
        project.parts.insert("PSG1_11223344".to_string(), vgm_part("PSG1_11223344"));
        project.patch_instruments.insert(
            "banks/song/FM1_aabbccdd.patch".to_string(),
            vgm_patch("fm.operator"),
        );
        project.patch_instruments.insert(
            "banks/song/PSG1_11223344.patch".to_string(),
            vgm_patch("tone.square"),
        );

        let (psg, fm) = chip_patch_groups(&project);
        assert_eq!(fm.len(), 1, "FM patch grouped as FM");
        assert_eq!(psg.len(), 1, "PSG patch grouped as PSG");
        // The bank key must be the mixer path (`part.sample_path`), which is
        // what the loader, TUI faders and mute/solo all use. Keying banks by
        // the halotext call name made notes audible but disconnected every
        // per-part volume and mute control.
        assert_eq!(fm[0].0, "banks/song/FM1_aabbccdd.patch");
        assert_eq!(psg[0].0, "banks/song/PSG1_11223344.patch");
        // And each key still resolves through the part map to its patch.
        assert!(project.patch_instruments.contains_key(&fm[0].0));
        assert!(project.patch_instruments.contains_key(&psg[0].0));
    }

    #[test]
    fn sf2_mixer_paths_use_part_path_not_alias() {
        let mut project = Project::empty(48_000);
        let spec = Sf2EngineSpec {
            sf2_path: PathBuf::from("samples/Titanic.sf2"),
            bank: 0,
            preset: 6,
            gain: 1.0,
            iid: 12,
        };
        project.sf2_engine_specs.insert(
            "samples/Titanic.sf2#6".into(),
            spec.clone(),
        );
        project.sf2_engine_specs.insert("Titanic.sf2#6".into(), spec);

        let mut part = vgm_part("Titanic_3");
        part.sample_path = "samples/Titanic.sf2".into();
        part.preset = Some(6);
        project.parts.insert("Titanic_3".into(), part);

        let paths = sf2_mixer_paths(&project);
        assert_eq!(
            paths.get(&12).map(String::as_str),
            Some("samples/Titanic.sf2")
        );
    }

    #[test]
    fn vst_mixer_paths_ignore_aliases() {
        let mut project = Project::empty(48_000);
        let inst = Arc::new(VstInstrument {
            iid: 7,
            name: "Surge XT".into(),
            plugin_path: PathBuf::from("Surge XT.vst3"),
            state_bytes: Vec::new(),
            fallback: true,
            gain: 1.0,
        });
        project.vst_instruments.insert(
            "instruments/Sawer.instrument".into(),
            Arc::clone(&inst),
        );
        project.vst_instruments.insert("Sawer.vst3".into(), Arc::clone(&inst));
        project.vst_instruments.insert("Surge XT.vst3".into(), inst);

        let mut part = vgm_part("Sawer");
        part.sample_path = "Sawer.vst3".into();
        project.parts.insert("Sawer".into(), part);

        let paths = vst_mixer_paths(&project);
        assert_eq!(paths.get(&7).map(String::as_str), Some("Sawer.vst3"));
    }
}
