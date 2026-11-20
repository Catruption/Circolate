//! Offline WAV rendering.

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

use super::engine::Engine;
use super::project::{Cache, Loader, Project};
use super::runtime::{find_share_dir, install_all, InstallOptions};
use crate::mixer::state::MixerState;

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
    // A render depends on the project, not on whatever faders the TUI
    // happened to leave in the global mixer state.
    let mut engine = Engine::new(project, Arc::new(MixerState::default()));

    let fb = vst_fallback_gain();
    let share_dir = find_share_dir();
    let report = install_all(
        &mut engine,
        project,
        &InstallOptions {
            no_master_fx,
            sf2_override,
            fallback_gain: fb,
            share_dir: share_dir.as_deref(),
        },
    );
    crate::log_event!(
        "player.render.vst_hosts_installed",
        count: report.vst.loaded,
    );
    if (fb - 1.0).abs() > 1e-4 {
        crate::log_event!("player.render.vst_fallback_gain", gain: fb);
    }

    let first = loader.seek(start, start + lookahead);
    engine.apply_seek(start, first.samples);
    engine.ingest_vst(first.vsts);
    engine.ingest_chip(first.chips);

    let total = (project.total_samples - start) as usize;
    let mut l = vec![0f32; total];
    let mut r = vec![0f32; total];
    let mut i = 0;
    while i < total {
        let n = (total - i).min(BLOCK);
        let batch = loader.load_due(engine.pos + lookahead);
        engine.ingest(batch.samples);
        engine.ingest_vst(batch.vsts);
        engine.ingest_chip(batch.chips);
        engine.render(&mut l[i..i + n], &mut r[i..i + n], 1, project.stereo_width);
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

/// Write a 16-bit stereo WAV and return `(clipped_samples, peak)` so the
/// CLI can log a real end-of-render summary instead of only a file path.
pub(crate) fn write_wav(path: &Path, l: &[f32], r: &[f32], sr: u32) -> Result<(usize, f32)> {
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
    Ok((clipped as usize, peak))
}
