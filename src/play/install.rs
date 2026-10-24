//! Attaching instruments and effects to the audio `Engine`.
//!
//! Every `install_*` function operates on a live `player::Engine` and
//! attaches one kind of host: VST3 instruments, native SF2 engines,
//! master FX chain, or effect shims (fallback VST3s for effects the
//! trackscript references but the user doesn't have).
//!
//! **Plugin resolution is by name only.**  Recorded `Path:` fields are
//! never consulted — see the module docs on `crate::paths` for the
//! reasoning.  The transitional bridge for old trackscripts (which
//! still carry a `Path:` but no `plugin_name` field on
//! `VstInstrument`) is to derive the name from
//! `plugin_path.file_stem()`; the path itself is never printed or
//! loaded.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::deps::index::PluginIndex;
use crate::effects::EffectChain;
use crate::hosting::vst::ffi::VstHost;
use crate::paths;
use crate::player::{self, Engine};

// ── VST3 instruments ──────────────────────────────────────────────────

/// Attach every VST3 instrument the trackscript references.
///
/// Name resolution:
///
///   1. `vinst.plugin_name` if the parser populates it (future)
///   2. `plugin_path.file_stem()` — the plugin bundle name from the
///      recorded path, e.g. `"Odin2"` from
///      `.../Odin2.4.1Linux/Odin2.vst3`.  This is the transitional
///      bridge for old trackscripts.
///   3. `vinst.name` — the *part* name, as a last resort.  Usually
///      wrong (part names like `"ZENOLOGY_2"` don't match bundle
///      names), but harmless to try.
///
/// The path is never printed.  If the plugin is missing, the error
/// names the plugin, not a filesystem location — no dox, no leaking
/// the original author's home directory into logs.
pub fn install_vst_hosts(engine: &mut Engine, project: &player::Project) {
    if project.vst_instruments.is_empty() {
        return;
    }

    let index = PluginIndex::scan();

    for (name, vinst) in &project.vst_instruments {
        let stem = vinst
        .plugin_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

        let resolved = index
        .find_vst3(stem)
        .or_else(|| index.find_vst3(name))
        .cloned();

        let Some(path) = resolved else {
            eprintln!(
                "vst [{}] not found (looked up by name {:?})",
                      name, stem
            );
            continue;
        };

        match VstHost::load(path.to_str().unwrap_or(""), &vinst.state_bytes, project.sample_rate) {
            Ok(host) => {
                engine.install_host_with_gain(vinst.iid, host, vinst.gain);
                engine.register_part_path(vinst.iid, name.clone());
            }
            Err(e) => {
                eprintln!("vst [{}] failed to load: {:#}", name, e);
            }
        }
    }
}

// ── native SF2 engines ────────────────────────────────────────────────

pub fn install_sf2_engines(
    engine: &mut Engine,
    project: &player::Project,
    sf2_override: Option<&Path>,
) {
    let sf2_debug = std::env::var_os("FLP_DEBUG").is_some();
    let default_gain = crate::hosting::sf2::engine::DEFAULT_GAIN;
    for (name, spec) in &project.sf2_engine_specs {
        let path = sf2_override.unwrap_or(&spec.sf2_path);

        let mut sf2 = crate::hosting::sf2::engine::Sf2Engine::new(project.sample_rate);
        if let Err(e) = sf2.load(&path) {
            eprintln!(
                "sf2 [{}]: failed to load {}: {:#}",
                name,
                path.display(),
                      e,
            );
            continue;
        }
        sf2.set_preset(spec.bank, spec.preset);
        let effective = default_gain * spec.gain;
        if sf2_debug {
            eprintln!(
                "  sf2 [iid={:>3}] {:<44} DEFAULT_GAIN={:.3} spec.gain={:.3} effective={:.3}",
                spec.iid, name, default_gain, spec.gain, effective
            );
        }
        sf2.set_gain(effective);
        engine.install_sf2_engine(spec.iid, sf2);
        engine.register_part_path(spec.iid, name.clone());
    }
}

// ── master FX ─────────────────────────────────────────────────────────

pub fn install_master_fx(engine: &mut Engine, project: &player::Project, disabled: bool) {
    if disabled {
        return;
    }
    if let Ok(chain) = EffectChain::load_default(project.sample_rate) {
        if !chain.is_empty() {
            engine.install_master_fx(chain);
        }
    }
}

// ── share dir helper ──────────────────────────────────────────────────

pub fn find_share_dir() -> Option<PathBuf> {
    let p = paths::system_data();
    if p.join("fallbacks.json").is_file() {
        Some(p)
    } else {
        None
    }
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
///
/// `fallbacks.json` maps a missing plugin (e.g. `"Fruity Reeverb 2"`)
/// to a substitute name (e.g. `"Dragonfly Hall Reverb"`).  The
/// inventory is no longer consulted for a *path* — the substitute name
/// is passed through `paths::find_plugin`, the same resolver every
/// other load site uses.
fn resolve_effect_path(target: &str, share_dir: &Path) -> Option<PathBuf> {
    let debug = std::env::var_os("SHIM_DEBUG").is_some();
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

    if debug {
        eprintln!(
            "shim-debug: {:?} → {:?}",
            target_norm, display_name
        );
    }

    let resolved = paths::find_plugin(display_name);
    if resolved.is_none() && debug {
        eprintln!(
            "shim-debug: fallback {:?} not installed on this machine",
            display_name
        );
    }
    resolved
}

pub fn install_shims(
    engine: &mut Engine,
    project: &player::Project,
    share_dir: Option<&Path>,
) {
    if project.shims.is_empty() {
        return;
    }
    let debug = std::env::var_os("SHIM_DEBUG").is_some();
    let Some(share_dir) = share_dir else {
        eprintln!(
            "  shims: share/ not found; {} shim(s) skipped",
                  project.shims.len()
        );
        return;
    };
    if debug {
        eprintln!(
            "shim-debug: share_dir={:?} ({} shim(s) to attach)",
                  share_dir,
                  project.shims.len()
        );
    }

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
            skipped += 1;
            continue;
        };

        let mut host = match VstHost::load(path.to_str().unwrap_or(""), &[], project.sample_rate) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("  shim: failed to load '{}': {:#}", shim.target, e);
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
            if debug {
                eprintln!(
                    "shim-debug: set_param({} idx={} val={:.3}) = {}",
                          shim.target, index, normalized, ok
                );
            }
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
        eprintln!(
            "  shim {:02}: loaded '{}'",
            id, shim.target,
        );
    }

    eprintln!(
        "  shims: {} attachment(s), {} skipped, {} unique VST3 host(s) (from {} shim event(s))",
              attached, skipped, next_shim_id, total_shims,
    );
}
