//! Non-TUI paths: `--render` and `--verify-cache`.
//!
//! Neither touches cpal or the terminal; both call `player::load_project`
//! and `player::render_offline` directly.

use anyhow::{anyhow, bail, Result};

use crate::player;

use super::cli::Args;
use super::resolve::resolve_source;

pub fn verify_cache(args: &Args) -> Result<()> {
    let song = args
    .song
    .as_ref()
    .ok_or_else(|| anyhow!("--verify-cache needs a song file"))?;
    let rate = args.rate;
    let (path, _guard) = resolve_source(song, args)?;
    let (p1, c1) = player::load_project(&path, rate, true, false)?;
    let (a_l, a_r, c1) = player::render_offline(&p1, c1, 0, true, None);
    println!("  with cache:    {}", c1.describe());
    let (p2, c2) = player::load_project(&path, rate, false, false)?;
    let (b_l, b_r, c2) = player::render_offline(&p2, c2, 0, true, None);
    println!("  without cache: {}", c2.describe());
    let diffs = |a: &[f32], b: &[f32]| {
        a.iter()
        .zip(b)
        .filter(|(x, y)| x.to_bits() != y.to_bits())
        .count()
    };
    let n = diffs(&a_l, &b_l) + diffs(&a_r, &b_r);
    if a_l.len() == b_l.len() && n == 0 {
        println!("cache check OK: {} frames identical", a_l.len());
        Ok(())
    } else {
        bail!(
            "cache check FAILED: {} differing samples (lengths {} vs {})",
              n,
              a_l.len(),
              b_l.len()
        )
    }
}

pub fn render(args: &Args) -> Result<()> {
    let out = args
    .render
    .as_ref()
    .expect("mod.rs only calls offline::render when args.render is Some");
    let song = args
    .song
    .as_ref()
    .ok_or_else(|| anyhow!("--render needs a song file"))?;
    let (path, _guard) = resolve_source(song, args)?;
    let (project, cache) =
    player::load_project(&path, args.rate, !args.no_cache, false)?;
    let t0 = std::time::Instant::now();
    let start = (args.start * args.rate as f64) as i64;
    let (l, r, cache) = player::render_offline(
        &project,
        cache,
        start,
        args.no_master_fx,
        args.sf2_override.as_deref(),
    );
    player::write_wav(out, &l, &r, args.rate)?;
    println!(
        "  wrote {} ({:.2}s of audio in {:.1}s; {})",
             out.display(),
             l.len() as f64 / args.rate as f64,
             t0.elapsed().as_secs_f64(),
             cache.describe()
    );
    Ok(())
}
