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
            crate::log_event!(
                "player.render.vst_skipped",
                name: name.clone(),
                              stem: format!("{:?}", stem),
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
                let display_name = if stem.is_empty() { name.as_str() } else { stem };
                crate::log_event!(
                    "player.render.vst_loaded",
                    iid: vinst.iid,
                    name: display_name,
                    tag: tag,
                );
                engine.install_host_with_gain(vinst.iid, host, eff_gain);
                engine.register_part_path(vinst.iid, name.clone());
            }
            Err(e) => {
                crate::log_event!(
                    "player.render.vst_load_failed",
                    name: name.clone(),
                                  err: format!("{:#}", e),
                );
            }
        }
    }
    crate::log_event!(
        "player.render.vst_hosts_installed",
        count: engine.host_count(),
    );
    if (fb - 1.0).abs() > 1e-4 {
        crate::log_event!("player.render.vst_fallback_gain", gain: fb);
    }

    let sf2_debug = std::env::var_os("FLP_DEBUG").is_some();
    let default_gain = crate::hosting::sf2::engine::DEFAULT_GAIN;
    for (name, spec) in &project.sf2_engine_specs {
        let path = sf2_override.unwrap_or(&spec.sf2_path);
        let mut sf2 = Sf2Engine::new(project.sample_rate);
        if let Err(e) = sf2.load(path) {
            crate::log_event!(
                "player.render.sf2_load_failed",
                name: name.clone(),
                              err: format!("{:#}", e),
            );
            continue;
        }
        sf2.set_preset(spec.bank, spec.preset);
        let effective = default_gain * spec.gain;
        if sf2_debug {
            crate::log_event!(
                "player.render.sf2_debug",
                body: format!(
                    "  sf2 [iid={:>3}] {:<44} DEFAULT_GAIN={:.3} spec.gain={:.3} effective={:.3}",
                    spec.iid, name, default_gain, spec.gain, effective,
                ),
            );
        }
        sf2.set_gain(effective);
        engine.install_sf2_engine(spec.iid, sf2);
        engine.register_part_path(spec.iid, name.clone());
    }
    crate::log_event!(
        "player.render.sf2_engines_installed",
        count: engine.sf2_engine_count(),
    );

    crate::player::inserts::install_chains(&mut engine, project, project.sample_rate);

    if !no_master_fx {
        match EffectChain::load_default(project.sample_rate) {
            Ok(chain) => {
                if !chain.is_empty() {
                    crate::log_event!(
                        "player.master_fx.loaded",
                        name: chain.describe(),
                    );
                }
                engine.install_master_fx(chain);
            }
            Err(e) => {
                crate::log_event!(
                    "player.master_fx.chain_failed",
                    err: format!("{:#}", e),
                );
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
    let cache = Arc::try_unwrap(loader.cache)
    .ok()
    .expect("offline render should own the cache exclusively")
    .into_inner()
    .ok()
    .expect("cache mutex poisoned");
    (l, r, cache)
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
        crate::log_event!(
            "player.render.wav_clipped",
            clipped: clipped,
            peak: peak,
        );
    }
    Ok(())
}
