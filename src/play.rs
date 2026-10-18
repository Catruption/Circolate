//! `trackscript-player play` — play a song, or render it to WAV offline.
//!
//! Live playback owns the terminal for the entire session: the TUI comes
//! up once, stays up across song changes, and only tears down on quit.

use anyhow::{anyhow, bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::Stdout;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::archive;
use crate::convert::{self, ConvertOptions};
use crate::effects::EffectChain;
use crate::player::{self, Cache, Engine, Loader, Msg, Project, Transport};
use crate::formats::SourceKind;
use crate::tui;
use crate::tui::config::{Config, Settings};
use crate::tui::state as queue;
use crate::hosting::vst::ffi::VstHost;

// ── silence guard ──────────────────────────────────────────────────────

#[cfg(unix)]
mod silence {
    pub struct Silence {
        saved_out: i32,
        saved_err: i32,
    }

    impl Silence {
        pub fn new() -> Option<Self> {
            unsafe {
                let saved_out = libc::dup(1);
                let saved_err = libc::dup(2);
                if saved_out < 0 || saved_err < 0 {
                    if saved_out >= 0 { libc::close(saved_out); }
                    if saved_err >= 0 { libc::close(saved_err); }
                    return None;
                }
                let devnull = libc::open(
                    b"/dev/null\0".as_ptr() as *const libc::c_char,
                    libc::O_WRONLY,
                );
                if devnull < 0 {
                    libc::close(saved_out);
                    libc::close(saved_err);
                    return None;
                }
                libc::dup2(devnull, 1);
                libc::dup2(devnull, 2);
                libc::close(devnull);
                Some(Silence { saved_out, saved_err })
            }
        }
    }

    impl Drop for Silence {
        fn drop(&mut self) {
            unsafe {
                libc::dup2(self.saved_out, 1);
                libc::dup2(self.saved_err, 2);
                libc::close(self.saved_out);
                libc::close(self.saved_err);
            }
        }
    }
}

#[cfg(not(unix))]
mod silence {
    pub struct Silence;
    impl Silence {
        pub fn new() -> Option<Self> { Some(Silence) }
    }
}

// ── CLI ────────────────────────────────────────────────────────────────

struct Args {
    song: Option<PathBuf>,
    render: Option<PathBuf>,
    rate: u32,
    start: f64,
    no_cache: bool,
    verify: bool,
    samples_dir: Option<PathBuf>,
    skip_native: bool,
    no_master_fx: bool,
    no_watch: bool,
    /// `--sf2 PATH`: override whatever SF2 the sidecars reference.  Used
    /// by `run.sh` when the user picks a bonus SoundFont.
    sf2_override: Option<PathBuf>,
}

fn parse_args(argv: &[String]) -> Result<Args> {
    let mut it = argv.iter();
    let mut song: Option<PathBuf> = None;
    let mut render = None;
    let mut rate = 44100u32;
    let mut start = 0.0f64;
    let (mut no_cache, mut verify) = (false, false);
    let mut samples_dir: Option<PathBuf> = None;
    let mut skip_native = false;
    let mut no_master_fx = false;
    let mut no_watch = false;
    let mut sf2_override: Option<PathBuf> = None;
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
            "--sf2" => {
                sf2_override = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("--sf2 needs a path"))?,
                ))
            }
            "--skip-native" => skip_native = true,
            "--no-cache" => no_cache = true,
            "--verify-cache" => verify = true,
            "--no-master-fx" => no_master_fx = true,
            "--no-watch" => no_watch = true,
            _ if song.is_none() => song = Some(PathBuf::from(a)),
            _ => bail!("unexpected argument {a}"),
        }
    }
    Ok(Args {
        song, render, rate, start, no_cache, verify,
        samples_dir, skip_native, no_master_fx, no_watch,
        sf2_override,
    })
}

// ── helpers ────────────────────────────────────────────────────────────

fn is_archive(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map_or(false, |e| e.eq_ignore_ascii_case(archive::EXTENSION))
}

fn resolve_song(name: &str) -> Result<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        bail!("empty song name");
    }

    let direct = Path::new(name);
    if direct.is_file() {
        return Ok(direct.to_path_buf());
    }

    let dir = Path::new("trackscripts");
    for ext in ["tsz", "trackscript", "flp", "it"] {
        let candidate = dir.join(format!("{}.{}", name, ext));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    let candidate = dir.join(name);
    if candidate.is_file() {
        return Ok(candidate);
    }

    if let Ok(exe) = std::env::current_exe() {
        let mut d = exe.parent().map(Path::to_path_buf);
        while let Some(p) = d {
            let ts_dir = p.join("trackscripts");
            for ext in ["tsz", "trackscript", "flp", "it"] {
                let candidate = ts_dir.join(format!("{}.{}", name, ext));
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
            let candidate = ts_dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
            d = p.parent().map(Path::to_path_buf);
        }
    }

    bail!("no such song: {:?} (looked in trackscripts/)", name)
}

// ── temporary bundle ───────────────────────────────────────────────────

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
    if is_archive(src) {
        return Ok((src.to_path_buf(), None));
    }
    match SourceKind::from_path(src)? {
        SourceKind::Trackscript => Ok((src.to_path_buf(), None)),
        _ => {
            let mut opts = ConvertOptions::default();
            if let Some(d) = &args.samples_dir {
                opts.samples_dir = Some(d.clone());
            }
            if let Some(s) = &args.sf2_override {
                opts.sf2_path = Some(s.clone());
            }
            opts.skip_native = args.skip_native;

            let conv = {
                let _s = silence::Silence::new();
                convert::convert_file(src, &opts)?
            };

            let tb = TempBundle::new("play")?;
            let stem = convert::output_stem(src);
            let ts = tb.write(&conv, &stem)?;
            if let Some(parent) = src.parent() {
                for name in ["cover.jpg", "cover.jpeg", "cover.png", "cover.webp"] {
                    let candidate = parent.join(name);
                    if candidate.is_file() {
                        let _ = std::fs::copy(&candidate, tb.dir.join(name));
                        break;
                    }
                }
            }
            Ok((ts, Some(tb)))
        }
    }
}

// ── install helpers ────────────────────────────────────────────────────

fn install_vst_hosts(engine: &mut Engine, project: &player::Project) {
    for (name, vinst) in &project.vst_instruments {
        let Some(path_str) = vinst.plugin_path.to_str() else { continue };
        match VstHost::load(path_str, &vinst.state_bytes, project.sample_rate) {
            Ok(host) => {
                engine.install_host_with_gain(vinst.iid, host, vinst.gain);
            }
            Err(_) => {
                let _ = name;
            }
        }
    }
}

/// Install a native SF2 engine for every `Sf2EngineSpec` in the project.
/// `sf2_override` (from `--sf2`) replaces the SF2 each spec references;
/// the spec's bank/patch/gain are preserved either way.
///
/// The engine must be `.load()`ed with the file path before
/// `install_sf2_engine` hands it to the mixer.  Constructing the engine,
/// setting a preset, and installing it without loading leaves it silent.
fn install_sf2_engines(
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
    }
}

fn install_master_fx(engine: &mut Engine, project: &player::Project, disabled: bool) {
    if disabled {
        return;
    }
    if let Ok(chain) = EffectChain::load_default(project.sample_rate) {
        if !chain.is_empty() {
            engine.install_master_fx(chain);
        }
    }
}

// ── live-reload watcher ────────────────────────────────────────────────

struct ReloadSlot {
    pending: Mutex<Option<(Arc<Project>, Cache)>>,
}

fn watch_for_changes(
    path: PathBuf,
    rate: u32,
    use_cache: bool,
    slot: Arc<ReloadSlot>,
    tp: Arc<Transport>,
    mut last_hashes: std::collections::HashMap<String, u64>,
) {
    let mut last_mtime = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok();

    while !tp.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(400));
        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok();
        if mtime == last_mtime {
            continue;
        }
        std::thread::sleep(Duration::from_millis(300));
        let settled = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok();
        if settled != mtime {
            continue;
        }
        let _s = silence::Silence::new();
        match player::load_project(&path, rate, use_cache, true) {
            Ok((project, cache)) => {
                let new_hashes = project.part_hash_map().clone();
                if new_hashes == last_hashes {
                    last_mtime = mtime;
                    continue;
                }
                last_hashes = new_hashes;
                if let Ok(mut g) = slot.pending.lock() {
                    *g = Some((project, cache));
                }
                last_mtime = mtime;
            }
            Err(_) => {}
        }
    }
}

// ── loader worker ──────────────────────────────────────────────────────

fn worker(
    mut loader: Loader,
    tx: Sender<Msg>,
    tp: Arc<Transport>,
    lookahead: i64,
    slot: Option<Arc<ReloadSlot>>,
    total: i64,
    wrap_tx: Sender<Msg>,
) {
    let mut epoch = 0u64;
    let mut wrap_ready = false;
    let mut last_wraps = tp.loop_wraps.load(Ordering::Acquire);
    let tail_margin = lookahead * 8;
    while !tp.stop.load(Ordering::Relaxed) {
        let wraps = tp.loop_wraps.load(Ordering::Acquire);
        if wraps != last_wraps {
            last_wraps = wraps;
            wrap_ready = false;
        }
        if let Some(slot) = &slot {
            let pending = slot.pending.lock().ok().and_then(|mut g| g.take());
            if let Some((new_project, new_cache)) = pending {
                wrap_ready = false;
                loader = Loader::new(new_project, new_cache);
                let cur = tp.position.load(Ordering::Relaxed);
                tp.seek_to.store(cur, Ordering::Release);
                tp.reload_count.fetch_add(1, Ordering::Relaxed);
            }
        }
        let req = tp.seek_to.swap(i64::MIN, Ordering::AcqRel);
        if req != i64::MIN {
            wrap_ready = false;
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
        } else if !wrap_ready {
            let pos = tp.position.load(Ordering::Relaxed);
            let horizon = pos + lookahead;
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

            if tp.loop_next.load(Ordering::Acquire)
                && tp.speed.load(Ordering::Relaxed) == 1
                && total > 0
                && pos + tail_margin >= total
            {
                let tail = loader.load_due(i64::MAX);
                if !(tail.samples.is_empty() && tail.vsts.is_empty()) {
                    let msg = Msg::Batch {
                        epoch,
                        samples: tail.samples,
                        vsts: tail.vsts,
                    };
                    if tx.send(msg).is_err() {
                        break;
                    }
                }
                epoch += 1;
                let first = loader.seek(0, lookahead);
                let wrap = Msg::Seek {
                    epoch,
                    pos: 0,
                    samples: first.samples,
                    vsts: first.vsts,
                };
                if wrap_tx.send(wrap).is_err() {
                    break;
                }
                wrap_ready = true;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

// ── verify-cache ───────────────────────────────────────────────────────

fn verify_cache(args: &Args, song: &Path) -> Result<()> {
    let rate = args.rate;
    let (path, _guard) = resolve_source(song, args)?;
    let (p1, c1) = player::load_project(&path, rate, true, false)?;
    let (a_l, a_r, c1) = player::render_offline(&p1, c1, 0, true, None);
    println!("  with cache:    {}", c1.describe());
    let (p2, c2) = player::load_project(&path, rate, false, false)?;
    let (b_l, b_r, c2) = player::render_offline(&p2, c2, 0, true, None);
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
            n, a_l.len(), b_l.len()
        )
    }
}

// ── one session ────────────────────────────────────────────────────────

enum Outcome {
    Quit,
    Next {
        name: String,
        queue: queue::QueueState,
        notes: Vec<String>,
    },
}

fn play_one(
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    args: &Args,
    song: Option<&Path>,
    queue: queue::QueueState,
    settings: Settings,
) -> Result<Outcome> {
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

    // Prefer 48000 Hz when the device supports it.  PipeWire's house
    // clock is 48000 on almost every modern desktop, and every stream
    // we open at 44100 gets resampled at the graph boundary.  Chiptune
    // (square waves, high harmonics) is the worst case for that: the
    // resampler's lowpass aliases content the ear reads as buzz.
    // cpal's ALSA backend on a pipewire-alsa system reports 44100 from
    // the default PCM, so we override here rather than trusting the
    // device default.
    let mut stream_config: cpal::StreamConfig = device_config.clone().into();
    let preferred = cpal::SampleRate(48000);
    let supports_preferred = device
        .supported_output_configs()
        .map(|cfgs| {
            cfgs.into_iter().any(|c| {
                c.channels() == stream_config.channels
                    && c.sample_format() == cpal::SampleFormat::F32
                    && c.min_sample_rate() <= preferred
                    && preferred <= c.max_sample_rate()
            })
        })
        .unwrap_or(false);

    if supports_preferred && device_config.sample_rate() != preferred {
        stream_config.sample_rate = preferred;
    }
    let sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    let is_watchable_trackscript = song.map_or(false, |p| {
        !is_archive(p)
            && matches!(SourceKind::from_path(p), Ok(SourceKind::Trackscript))
    });

    let (project, cache, project_path, _guard) = match song {
        Some(p) => {
            let (ts_path, guard) = resolve_source(p, args)?;
            let (project, cache) = {
                let _s = silence::Silence::new();
                player::load_project(&ts_path, sample_rate, !args.no_cache, true)?
            };
            (project, cache, ts_path, guard)
        }
        None => {
            let project = Arc::new(Project::empty(sample_rate));
            let cache = Cache::new(!args.no_cache);
            (project, cache, PathBuf::new(), None)
        }
    };

    let mut queue = queue;
    if queue.playing.is_none() {
        if let Some(name) = song
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
        {
            let row = queue.cursor.row.min(queue.lines.len().saturating_sub(1));
            if queue.lines.get(row).map_or(false, |l| l.is_blank()) {
                let text: String = name.chars().take(queue::TEXT_W as usize).collect();
                queue.lines[row].text = text;
                queue.start_at(row);
            }
        }
    }

    let total = project.total_samples;
    let start = ((args.start * sample_rate as f64) as i64).clamp(0, total);
    let initial_speed: i64 = if project.voice_count > 0 { 1 } else { 0 };

    let transport = Arc::new(Transport {
        speed: AtomicI64::new(initial_speed),
        seek_to: AtomicI64::new(start),
        position: AtomicI64::new(start),
        done: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        active_voices: AtomicI64::new(0),
        loaded_lines: AtomicI64::new(0),
        reload_count: AtomicI64::new(0),
        loop_next: AtomicBool::new(false),
        loop_wraps: AtomicI64::new(0),
    });

    let reload_slot = if is_watchable_trackscript && !args.no_watch {
        let slot = Arc::new(ReloadSlot {
            pending: Mutex::new(None),
        });
        let initial_hashes = project.part_hash_map().clone();
        let watch_tp = Arc::clone(&transport);
        let watch_slot = Arc::clone(&slot);
        let watch_path = project_path.clone();
        let watch_rate = sample_rate;
        let watch_use_cache = !args.no_cache;
        std::thread::spawn(move || {
            watch_for_changes(
                watch_path,
                watch_rate,
                watch_use_cache,
                watch_slot,
                watch_tp,
                initial_hashes,
            )
        });
        Some(slot)
    } else {
        None
    };

    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = channel();
    let (wrap_tx, wrap_rx): (Sender<Msg>, Receiver<Msg>) = channel();
    let lookahead = sample_rate as i64 / 4;
    {
        let loader = Loader::new(Arc::clone(&project), cache);
        let tp = Arc::clone(&transport);
        let slot = reload_slot.clone();
        std::thread::spawn(move || worker(loader, tx, tp, lookahead, slot, total, wrap_tx));
    }

    let mut engine = Engine::new(&project);
    install_vst_hosts(&mut engine, &project);
    install_sf2_engines(&mut engine, &project, args.sf2_override.as_deref());
    install_master_fx(&mut engine, &project, args.no_master_fx);

    let tp = Arc::clone(&transport);
    let mut left = vec![0f32; 4096];
    let mut right = vec![0f32; 4096];
    let mut pending_wrap: Option<Msg> = None;
    let mut cur_epoch = 0u64;

    let stream = device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _| {
            // ── Ingest ─────────────────────────────────────────────
            while let Ok(m) = rx.try_recv() {
                if let Msg::Seek { epoch, .. } = &m {
                    cur_epoch = *epoch;
                    pending_wrap = None;
                }
                engine.handle(m);
            }
            while let Ok(m) = wrap_rx.try_recv() {
                if let Msg::Seek { epoch, .. } = &m {
                    if *epoch > cur_epoch {
                        pending_wrap = Some(m);
                    }
                }
            }

            let speed = tp.speed.load(Ordering::Relaxed);
            let loop_next = tp.loop_next.load(Ordering::Acquire);

            // ── End-of-song fallback ───────────────────────────────
            if !engine.ready && speed == 1 && loop_next && pending_wrap.is_some() {
                if let Some(m) = pending_wrap.take() {
                    if let Msg::Seek { epoch, .. } = &m {
                        cur_epoch = *epoch;
                    }
                    engine.handle(m);
                    tp.loop_wraps.fetch_add(1, Ordering::Release);
                }
                tp.done.store(false, Ordering::Release);
            }

            // ── Normal readiness check ─────────────────────────────
            if !engine.ready {
                data.fill(0.0);
                return;
            }
            tp.position.store(engine.pos, Ordering::Relaxed);

            let can_wrap = speed == 1 && pending_wrap.is_some() && loop_next;
            let reached_end = speed > 0 && engine.finished() && !can_wrap;
            if reached_end {
                data.fill(0.0);
                tp.done.store(true, Ordering::Release);
                return;
            }
            if speed == 0 {
                data.fill(0.0);
                return;
            }
            tp.done.store(false, Ordering::Release);

            // ── Render ─────────────────────────────────────────────
            let frames = data.len() / channels;
            if left.len() < frames {
                left.resize(frames, 0.0);
                right.resize(frames, 0.0);
            }
            let (l, r) = (&mut left[..frames], &mut right[..frames]);
            let mut head = 0usize;
            if can_wrap {
                let rem = (total - engine.pos).max(0) as usize;
                if rem < frames {
                    if rem > 0 {
                        engine.render(&mut l[..rem], &mut r[..rem], speed);
                    }
                    if let Some(m) = pending_wrap.take() {
                        if let Msg::Seek { epoch, .. } = &m {
                            cur_epoch = *epoch;
                        }
                        engine.handle(m);
                        tp.loop_wraps.fetch_add(1, Ordering::Release);
                    }
                    head = rem;
                }
            }
            engine.render(&mut l[head..], &mut r[head..], speed);

            if engine.pos <= 0 && speed < 0 {
                tp.speed.store(0, Ordering::Release);
            }
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
        |_err| {},
        None,
    )?;

    stream.play()?;

    let notes: Vec<String> = project
        .requires
        .as_ref()
        .map(|r| crate::deps::requires::report_lines(r))
        .unwrap_or_default();

    let (request, queue) = tui::run_against(
        term,
        Arc::clone(&transport),
        Arc::clone(&project),
        sample_rate,
        queue,
        settings,
    )?;
    transport.stop.store(true, Ordering::Relaxed);
    drop(stream);

    match request {
        None => Ok(Outcome::Quit),
        Some(name) => Ok(Outcome::Next { name, queue, notes }),
    }
}

// ── entry point ────────────────────────────────────────────────────────

pub fn run(argv: &[String]) -> Result<()> {
    let args = parse_args(argv)?;

    if args.verify {
        let song = args
            .song
            .as_ref()
            .ok_or_else(|| anyhow!("--verify-cache needs a song file"))?;
        return verify_cache(&args, song);
    }

    if let Some(out) = &args.render {
        let song = args
            .song
            .as_ref()
            .ok_or_else(|| anyhow!("--render needs a song file"))?;
        let (path, _guard) = resolve_source(song, &args)?;
        let (project, cache) =
            player::load_project(&path, args.rate, !args.no_cache, false)?;
        let t0 = std::time::Instant::now();
        let start = (args.start * args.rate as f64) as i64;
        let (l, r, cache) = player::render_offline(
            &project, cache, start, args.no_master_fx,
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
        return Ok(());
    }

    let mut term = tui::setup()?;
    let mut song = args.song.clone();
    let mut queue = queue::QueueState::new();
    let mut pending_notes: Vec<String> = Vec::new();

    let result = (|| -> Result<()> {
        loop {
            let dir = song.as_deref().and_then(|p| p.parent());
            let settings = Config::load_layered(dir).resolve();

            let msg = match &song {
                Some(p) => format!("loading {}...", p.display()),
                None => "no song — queue box is focused; type a name and press enter".into(),
            };
            let notes: Vec<String> = pending_notes.drain(..).collect();
            tui::show_loading(&mut term, &queue, &settings, &msg, &notes)?;

            match play_one(&mut term, &args, song.as_deref(), queue.clone(), settings.clone()) {
                Ok(Outcome::Quit) => return Ok(()),
                Ok(Outcome::Next { name, queue: q, notes: n }) => {
                    queue = q;
                    pending_notes = n;
                    match resolve_song(&name) {
                        Ok(p) => song = Some(p),
                        Err(_) => song = None,
                    }
                }
                Err(e) => {
                    let msg = format!("load failed: {e:#}");
                    tui::show_loading(&mut term, &queue, &settings, &msg, &[])?;
                    std::thread::sleep(Duration::from_secs(3));
                    song = None;
                }
            }
        }
    })();

    let _ = tui::restore(&mut term);
    result
}
