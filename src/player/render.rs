//! Offline WAV rendering.

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

use super::engine::Engine;
use super::project::{Cache, Loader, Project};
use crate::deps::index::PluginIndex;
use crate::effects::EffectChain;
use crate::hosting::sf2::engine::Sf2Engine;
use crate::hosting::vst::ffi::VstHost;

const BLOCK: usize = 4096;

/// Environment-driven attenuation applied to *fallback* VST
/// substitutions only (never to exact matches).
///
/// Fallback substitutes (e.g. Surge XT standing in for `FNF Boyfriend`)
/// do not receive the original plugin's state blob — parameter IDs are
/// incompatible — so they run at their own defaults. Surge XT's default
/// patch is roughly 8–12 dB hotter than the level at which a typical
/// FL Studio project mixes a synth channel. Unlike the SF2 path, which
/// already multiplies in `hosting::sf2::engine::DEFAULT_GAIN`, fallback
/// VSTs currently have no equivalent attenuation stage.
///
/// Set `VST_FALLBACK_GAIN=0.30` (or similar) to bring fallback channels
/// into a listenable range while insert-effect reconstruction
/// (`share/native_fx/*.lut.bin`, `registry.json` fallback rules) is
/// still being built out. This is a stopgap — once fallbacks load real
/// state or are guided by an explicit per-plugin `gain` in the registry,
/// the env var can be retired (or the default retuned).
const DEFAULT_VST_FALLBACK_GAIN: f32 = 1.0;

fn vst_fallback_gain() -> f32 {
    std::env::var("VST_FALLBACK_GAIN")
    .ok()
    .and_then(|s| s.parse::<f32>().ok())
    .filter(|g| *g > 0.0 && g.is_finite())
    .unwrap_or(DEFAULT_VST_FALLBACK_GAIN)
}

pub(crate) fn render_offline(
    project: &Arc<Project>,
    cache: Cache,
    start: i64,
    no_master_fx: bool,
    sf2_override: Option<&Path>,
) -> (Vec<f32>, Vec<f32>, Cache) {
    let lookahead = project.sample_rate as i64;
    let start = start.clamp(0, project.total_samples);

    let mut loader = Loader::new(Arc::clone(project), cache);
    let mut engine = Engine::new(project);

    // Install VST hosts before any rendering.  One per channel iid.
    //
    // Name-only resolution: the recorded `plugin_path` is stale and may
    // leak the original author's home directory, so it's used solely as
    // a **transitional bridge** to recover the plugin name (via
    // `file_stem`).  The actual load path comes from the local plugin
    // scan.  New trackscripts that stop emitting `Path:` will need a
    // `plugin_name` field on `VstInstrument`; until then this is the
    // workable fallback for both old and new sidecars.
    //
    // Fallback hosts receive an extra attenuation stage driven by
    // VST_FALLBACK_GAIN (see the constant's doc comment). Exact
    // matches get `vinst.gain` verbatim — they carry the original
    // plugin and its state, so no fudge is needed.
    let fb = vst_fallback_gain();
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

        let Some(plugin_path) = resolved else {
            eprintln!(
                "  (skipping VST [{}]: plugin {:?} not installed)",
                      name, stem
            );
            continue;
        };

        let eff_gain = if vinst.fallback {
            vinst.gain * fb
        } else {
            vinst.gain
        };
        match VstHost::load(
            plugin_path.to_str().unwrap_or(""),
                            &vinst.state_bytes,
                            project.sample_rate,
        ) {
            Ok(host) => {
                let tag = if vinst.fallback {
                    if (eff_gain - 1.0).abs() > 1e-4 {
                        format!("fallback, gain {:.3}", eff_gain)
                    } else {
                        "fallback".to_string()
                    }
                } else {
                    "exact".to_string()
                };
                println!(
                    "  loaded VST [{}] {} ({})",
                         vinst.iid,
                         if stem.is_empty() { name.as_str() } else { stem },
                             tag
                );
                engine.install_host_with_gain(vinst.iid, host, eff_gain);
                engine.register_part_path(vinst.iid, name.clone());
            }
            Err(e) => {
                eprintln!("  failed to load VST {} ({:#})", name, e);
            }
        }
    }
    println!("  installed {} VST host(s)", engine.host_count());
    if (fb - 1.0).abs() > 1e-4 {
        println!("  VST fallback attenuation: {:.3} (VST_FALLBACK_GAIN)", fb);
    }

    // Native SF2 engines.  Same setup as `install_sf2_engines` in the live
    // path; without this, every SF2 note in an offline render is dropped
    // and the WAV comes out silent.
    let sf2_debug = std::env::var_os("FLP_DEBUG").is_some();
    let default_gain = crate::hosting::sf2::engine::DEFAULT_GAIN;
    for (name, spec) in &project.sf2_engine_specs {
        let path = sf2_override.unwrap_or(&spec.sf2_path);
        let mut sf2 = Sf2Engine::new(project.sample_rate);
        if let Err(e) = sf2.load(path) {
            eprintln!("  sf2 engine failed to load {}: {:#}", name, e);
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
    println!("  installed {} SF2 engine(s)", engine.sf2_engine_count());

    // Insert chains: VST effects on mixer inserts, loaded from the
    // `.instrument` sidecars the FLP converter wrote next to the
    // trackscript.  The directory is passed via TRACKSCRIPT_INSERT_DIR
    // for now; later this becomes a Project field.
    crate::player::inserts::install_chains(&mut engine, project, project.sample_rate);

    // Master bus effects.  Silent if master_fx.json is missing or empty.
    if !no_master_fx {
        match EffectChain::load_default(project.sample_rate) {
            Ok(chain) => {
                if !chain.is_empty() {
                    println!("  {}", chain.describe());
                }
                engine.install_master_fx(chain);
            }
            Err(e) => {
                eprintln!("  master fx: {:#}", e);
            }
        }
    }

    let first = loader.seek(start, start + lookahead);
    engine.apply_seek(start, first.samples);
    engine.ingest_vst(first.vsts);

    let total = (project.total_samples - start) as usize;
    let mut l = vec![0f32; total];
    let mut r = vec![0f32; total];
    let mut i = 0;
    while i < total {
        let n = (total - i).min(BLOCK);
        let batch = loader.load_due(engine.pos + lookahead);
        engine.ingest(batch.samples);
        engine.ingest_vst(batch.vsts);
        engine.render(&mut l[i..i + n], &mut r[i..i + n], 1);
        i += n;
    }
    (l, r, loader.cache)
}

pub(crate) fn write_wav(path: &Path, l: &[f32], r: &[f32], sr: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    let (mut clipped, mut peak) = (0u64, 0f32);
    for i in 0..l.len() {
        for s in [l[i], r[i]] {
            let a = s.abs();
            if a > 1.0 {
                clipped += 1;
            }
            peak = peak.max(a);
            writer.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
        }
    }
    writer.finalize()?;
    if clipped > 0 {
        println!(
            "  warning: {} output samples clip at peak {:.3}",
            clipped, peak
        );
    }
    Ok(())
}
