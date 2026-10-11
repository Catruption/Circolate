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
use crate::effects::EffectChain;
use crate::player::{self, Engine, Loader, Msg, Transport};
use crate::sources::SourceKind;
use crate::tui;
use crate::vst_host_ffi::VstHost;

struct Args {
    song: PathBuf,
    render: Option<PathBuf>,
    rate: u32,
    start: f64,
    no_cache: bool,
    verify: bool,
    samples_dir: Option<PathBuf>,
    skip_native: bool,
    no_master_fx: bool,
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
    let mut no_master_fx = false;
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
            "--no-master-fx" => no_master_fx = true,
            _ if song.is_none() => song = Some(PathBuf::from(a)),
            _ => bail!("unexpected argument {a}"),
        }
    }
    let song = song.ok_or_else(|| {
        anyhow!(
            "usage: trackscript-player play <song> \
[--start SEC] [--samples-dir DIR] [--skip-native] [--no-cache] [--verify-cache] \
[--no-master-fx] [--render out.wav [--rate HZ]]"
        )
    })?;
    Ok(Args {
        song, render, rate, start, no_cache, verify,
       samples_dir, skip_native, no_master_fx,
    })
}

/// A temporary directory that deletes itself on drop.
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

fn install_vst_hosts(engine: &mut Engine, project: &player::Project) {
    for (name, vinst) in &project.vst_instruments {
        let Some(path_str) = vinst.plugin_path.to_str() else {
            eprintln!("  (skipping VST with non-UTF8 path: {})", name);
            continue;
        };
        match VstHost::load(path_str, &vinst.state_bytes, project.sample_rate) {
            Ok(host) => {
                let tag = if vinst.fallback { "fallback" } else { "exact" };
                println!(
                    "  loaded VST [{}] {} ({})",
                         vinst.iid,
                         vinst.plugin_path
                         .file_stem()
                         .and_then(|s| s.to_str())
                         .unwrap_or("?"),
                         tag
                );
                engine.install_host(vinst.iid, host);
            }
            Err(e) => {
                eprintln!("  failed to load VST {} ({:#})", name, e);
            }
        }
    }
    println!("  installed {} VST host(s)", engine.host_count());
}

fn install_master_fx(engine: &mut Engine, project: &player::Project, disabled: bool) {
    if disabled {
        return;
    }
    match EffectChain::load_default(project.sample_rate) {
        Ok(chain) => {
            if !chain.is_empty() {
                println!("  {}", chain.describe());
            }
            engine.install_master_fx(chain);
        }
        Err(e) => eprintln!("  master fx: {:#}", e),
    }
}

fn worker(mut loader: Loader, tx: Sender<Msg>, tp: Arc<Transport>, lookahead: i64) {
    let mut epoch = 0u64;
    while !tp.stop.load(Ordering::Relaxed) {
        let req = tp.seek_to.swap(i64::MIN, Ordering::AcqRel);
        if req != i64::MIN {
            epoch += 1;
            let batch = loader.seek(req, req + lookahead);
            let msg = Msg::Seek {
                epoch,
                pos: req,
                samples: batch.samples,
                vsts: batch.vsts,
            };
            if tx.send(msg).is_err() {
                break;
            }
        } else {
            let horizon = tp.position.load(Ordering::Relaxed) + lookahead;
            let batch = loader.load_due(horizon);
            let empty = batch.samples.is_empty() && batch.vsts.is_empty();
            if !empty {
                let msg = Msg::Batch {
                    epoch,
                    samples: batch.samples,
                    vsts: batch.vsts,
                };
                if tx.send(msg).is_err() {
                    break;
                }
            }
            tp.loaded_lines.store(loader.cursor as i64, Ordering::Relaxed);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn verify_cache(args: &Args) -> Result<()> {
    let rate = args.rate;
    let (path, _guard) = resolve_source(&args.song, args)?;
    let (p1, c1) = player::load_project(&path, rate, true)?;
    let (a_l, a_r, c1) = player::render_offline(&p1, c1, 0, true);
    println!("  with cache:    {}", c1.describe());
    let (p2, c2) = player::load_project(&path, rate, false)?;
    let (b_l, b_r, c2) = player::render_offline(&p2, c2, 0, true);
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

    if let Some(out) = &args.render {
        let (path, _guard) = resolve_source(&args.song, &args)?;
        let (project, cache) = player::load_project(&path, args.rate, !args.no_cache)?;
        let t0 = std::time::Instant::now();
        let start = (args.start * args.rate as f64) as i64;
        let (l, r, cache) = player::render_offline(&project, cache, start, args.no_master_fx);
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
    install_vst_hosts(&mut engine, &project);
    install_master_fx(&mut engine, &project, args.no_master_fx);

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
