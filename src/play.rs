//! `trackscript-player play` — play a song, or render it to WAV offline.
//!
//! Accepts any supported source: `.trackscript` plays directly, `.flp` and
//! `.it` are converted to a temp directory first.

use anyhow::{anyhow, bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use crate::convert::{self, ConvertOptions};
use crate::display;
use crate::player::{self, Engine, Loader, Msg, Transport};
use crate::sources::SourceKind;
use crate::tui;

struct Args {
    song: PathBuf,
    render: Option<PathBuf>,
    rate: u32,
    start: f64,
    no_cache: bool,
    verify: bool,
    samples_dir: Option<PathBuf>,
    skip_native: bool,
}

fn parse_args(argv: &[String]) -> Result<Args> {
    let mut it = argv.iter();
    let mut song = None;
    let mut render = None;
    let mut rate = 44100u32;
    let mut start = 0.0f64;
    let (mut no_cache, mut verify) = (false, false);
    let mut samples_dir: Option<PathBuf> = None;
    let mut skip_native = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--render" => {
                render = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("--render needs a path"))?,
                ))
            }
            "--rate" => {
                rate = it
                .next()
                .ok_or_else(|| anyhow!("--rate needs a value"))?
                .parse()?
            }
            "--start" => {
                start = it
                .next()
                .ok_or_else(|| anyhow!("--start needs seconds"))?
                .parse()?
            }
            "--samples-dir" => {
                samples_dir = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("--samples-dir needs a path"))?,
                ))
            }
            "--skip-native" => skip_native = true,
            "--no-cache" => no_cache = true,
            "--verify-cache" => verify = true,
            _ if song.is_none() => song = Some(PathBuf::from(a)),
            _ => bail!("unexpected argument {a}"),
        }
    }
    let song = song.ok_or_else(|| {
        anyhow!(
            "usage: trackscript-player play <song> \
[--start SEC] [--samples-dir DIR] [--skip-native] [--no-cache] [--verify-cache] \
[--render out.wav [--rate HZ]]"
        )
    })?;
    Ok(Args { song, render, rate, start, no_cache, verify, samples_dir, skip_native })
}

/// A temporary directory that deletes itself on drop.
///
/// Used to hold the converted trackscript + samples when playing a source
/// that isn't already trackscript.  Nothing is written to disk for a plain
/// `.trackscript` input.
struct TempBundle {
    dir: PathBuf,
}

impl TempBundle {
    fn new(tag: &str) -> Result<Self> {
        let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!("circolate-{}-{}-{}", tag, std::process::id(), nanos));
        std::fs::create_dir_all(&dir)?;
        Ok(TempBundle { dir })
    }

    /// Write a Converted bundle and return the path to the .trackscript.
    fn write(&self, c: &convert::Converted, stem: &str) -> Result<PathBuf> {
        let ts_path = self.dir.join(format!("{}.trackscript", stem));
        std::fs::write(&ts_path, &c.trackscript)?;

        for s in &c.samples {
            let p = self.dir.join(&s.rel_path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, &s.wav)?;
        }
        for a in &c.aux {
            let p = self.dir.join(&a.rel_path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, &a.bytes)?;
        }
        if !c.samples_json.is_empty() {
            std::fs::write(self.dir.join("samples.json"), &c.samples_json)?;
        }
        Ok(ts_path)
    }
}

impl Drop for TempBundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Ensure we have a trackscript on disk.  Returns the path to play, plus
/// an optional TempBundle that must be kept alive for the duration.
fn resolve_source(src: &Path, args: &Args) -> Result<(PathBuf, Option<TempBundle>)> {
    match SourceKind::from_path(src)? {
        SourceKind::Trackscript => Ok((src.to_path_buf(), None)),
        _ => {
            let mut opts = ConvertOptions::default();
            if let Some(d) = &args.samples_dir {
                opts.samples_dir = Some(d.clone());
            }
            opts.skip_native = args.skip_native;
            let conv = convert::convert_file(src, &opts)?;
            // Show what the converter noted (skipped channels, warnings).
            let mut in_footer = false;
            for line in conv.trackscript.lines() {
                if line.starts_with("// ------") {
                    in_footer = true;
                } else if in_footer && line.starts_with("//") {
                    println!("  {}", line);
                }
            }
            let tb = TempBundle::new("play")?;
            let stem = convert::output_stem(src);
            let ts = tb.write(&conv, &stem)?;
            Ok((ts, Some(tb)))
        }
    }
}

fn worker(mut loader: Loader, tx: Sender<Msg>, tp: Arc<Transport>, lookahead: i64) {
    let mut epoch = 0u64;
    while !tp.stop.load(Ordering::Relaxed) {
        let req = tp.seek_to.swap(i64::MIN, Ordering::AcqRel);
        if req != i64::MIN {
            epoch += 1;
            let voices = loader.seek(req, req + lookahead);
            if tx.send(Msg::Seek { epoch, pos: req, voices }).is_err() {
                break;
            }
        } else {
            let horizon = tp.position.load(Ordering::Relaxed) + lookahead;
            let voices = loader.load_due(horizon);
            if !voices.is_empty() && tx.send(Msg::Batch { epoch, voices }).is_err() {
                break;
            }
            tp.loaded_lines.store(loader.cursor as i64, Ordering::Relaxed);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn verify_cache(args: &Args) -> Result<()> {
    let (song, rate) = (&args.song, args.rate);
    let _ = song;
    let (path, _guard) = resolve_source(&args.song, args)?;
    let (p1, c1) = player::load_project(&path, rate, true)?;
    let (a_l, a_r, c1) = player::render_offline(&p1, c1, 0);
    println!("  with cache:    {}", c1.describe());
    let (p2, c2) = player::load_project(&path, rate, false)?;
    let (b_l, b_r, c2) = player::render_offline(&p2, c2, 0);
    println!("  without cache: {}", c2.describe());

    let diffs = |a: &[f32], b: &[f32]| {
        a.iter().zip(b).filter(|(x, y)| x.to_bits() != y.to_bits()).count()
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

pub fn run(argv: &[String]) -> Result<()> {
    let args = parse_args(argv)?;
    println!("loading {}", args.song.display());

    if args.verify {
        return verify_cache(&args);
    }

    // Offline render: no device.
    if let Some(out) = &args.render {
        let (path, _guard) = resolve_source(&args.song, &args)?;
        let (project, cache) = player::load_project(&path, args.rate, !args.no_cache)?;
        let t0 = std::time::Instant::now();
        let start = (args.start * args.rate as f64) as i64;
        let (l, r, cache) = player::render_offline(&project, cache, start);
        player::write_wav(out, &l, &r, args.rate)?;
        println!(
            "  wrote {} ({:.2}s of audio in {:.1}s; {})",
                 out.display(),
                 l.len() as f64 / args.rate as f64,
                 t0.elapsed().as_secs_f64(),
                 cache.describe()
        );
        return Ok(());
    }

    // Live playback.
    let host = cpal::default_host();
    let device = host
    .default_output_device()
    .ok_or_else(|| anyhow!("no default output device"))?;
    let device_config = device.default_output_config()?;
    if device_config.sample_format() != cpal::SampleFormat::F32 {
        bail!(
            "default output format is {:?}; this player only drives f32 devices for now",
            device_config.sample_format()
        );
    }
    let sample_rate = device_config.sample_rate().0;
    let stream_config: cpal::StreamConfig = device_config.into();
    let channels = stream_config.channels as usize;
    println!(
        "output: {:?}  {} Hz  {} ch",
        device.name().unwrap_or_default(),
             sample_rate,
             channels
    );

    // Convert if needed; keep the guard alive until the end of the fn.
    let (project_path, _guard) = resolve_source(&args.song, &args)?;

    let (project, cache) = player::load_project(&project_path, sample_rate, !args.no_cache)?;
    println!("  {}", cache.describe());
    display::banner(&project, sample_rate);
    let total = project.total_samples;
    let start = ((args.start * sample_rate as f64) as i64).clamp(0, total);

    let transport = Arc::new(Transport {
        paused: AtomicBool::new(false),
                             seek_to: AtomicI64::new(start),
                             position: AtomicI64::new(start),
                             done: AtomicBool::new(false),
                             stop: AtomicBool::new(false),
                             active_voices: AtomicI64::new(0),
                             loaded_lines: AtomicI64::new(0),
    });

    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = channel();
    let lookahead = sample_rate as i64 / 4;
    {
        let loader = Loader::new(Arc::clone(&project), cache);
        let tp = Arc::clone(&transport);
        std::thread::spawn(move || worker(loader, tx, tp, lookahead));
    }

    let mut engine = Engine::new(&project);
    let tp = Arc::clone(&transport);
    let mut left = vec![0f32; 4096];
    let mut right = vec![0f32; 4096];

    let stream = device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _| {
            while let Ok(m) = rx.try_recv() {
                engine.handle(m);
            }
            if !engine.ready {
                data.fill(0.0);
                return;
            }
            tp.position.store(engine.pos, Ordering::Relaxed);

            if tp.paused.load(Ordering::Relaxed) || engine.finished() {
                data.fill(0.0);
                if engine.finished() {
                    tp.done.store(true, Ordering::Release);
                }
                return;
            }

            let frames = data.len() / channels;
            if left.len() < frames {
                left.resize(frames, 0.0);
                right.resize(frames, 0.0);
            }
            let (l, r) = (&mut left[..frames], &mut right[..frames]);
            engine.render(l, r);
            tp.active_voices.store(engine.active_count() as i64, Ordering::Relaxed);
            for i in 0..frames {
                let o = i * channels;
                data[o] = l[i].clamp(-1.0, 1.0);
                if channels >= 2 {
                    data[o + 1] = r[i].clamp(-1.0, 1.0);
                }
                for c in 2..channels {
                    data[o + c] = 0.0;
                }
            }
            tp.position.store(engine.pos, Ordering::Relaxed);
        },
        |err| eprintln!("stream error: {err}"),
                                            None,
    )?;

    stream.play()?;
    println!("playing — Ctrl-C to stop");
    tui::run(Arc::clone(&transport), Arc::clone(&project), sample_rate)?;
    transport.stop.store(true, Ordering::Relaxed);
    println!("done");
    Ok(())
}
