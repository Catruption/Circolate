//! Offline WAV rendering.

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

use super::engine::Engine;
use super::project::{Cache, Loader, Project};

const BLOCK: usize = 4096;

pub(crate) fn render_offline(
    project: &Arc<Project>,
    cache: Cache,
    start: i64,
) -> (Vec<f32>, Vec<f32>, Cache) {
    let lookahead = project.sample_rate as i64;
    let start = start.clamp(0, project.total_samples);
    let mut loader = Loader::new(Arc::clone(project), cache);
    let mut engine = Engine::new(project);
    let first = loader.seek(start, start + lookahead);
    engine.apply_seek(start, first);

    let total = (project.total_samples - start) as usize;
    let mut l = vec![0f32; total];
    let mut r = vec![0f32; total];
    let mut i = 0;
    while i < total {
        let n = (total - i).min(BLOCK);
        let v = loader.load_due(engine.pos + lookahead);
        engine.ingest(v);
        engine.render(&mut l[i..i + n], &mut r[i..i + n]);
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
        println!("  warning: {} output samples clip at peak {:.3}", clipped, peak);
    }
    Ok(())
}
