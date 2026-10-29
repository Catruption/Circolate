//! Live playback session: cpal device, engine install, worker thread,
//! TUI loop.  One session per song; `Outcome::Next` re-enters with the
//! queue's selection.

use anyhow::{anyhow, bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::Stdout;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::formats::SourceKind;
use crate::player::{self, Cache, Engine, Loader, Msg, Project, Transport};
use crate::tui::{self, app::LoopOutcome};
use crate::tui::config::Settings;
use crate::tui::state::TEXT_W;
use crate::tui::state as queue;

use super::audio::{soft_clip, Silence};
use super::cli::Args;
use super::install::{
    find_share_dir, install_master_fx, install_sf2_engines, install_shims,
    install_vst_hosts,
};
use super::resolve::{is_archive, resolve_source};
use super::worker::{watch_for_changes, worker, ReloadSlot};

pub fn play_one(
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    args: &Args,
    song: Option<&Path>,
    queue: queue::QueueState,
    settings: Settings,
) -> Result<LoopOutcome> {
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
        !is_archive(p) && matches!(SourceKind::from_path(p), Ok(SourceKind::Trackscript))
    });

    let (project, cache_owned, project_path, _guard) = match song {
        Some(p) => {
            let (ts_path, guard) = resolve_source(p, args)?;
            let (project, cache) = {
                let _s = Silence::new();
                player::load_project(&ts_path, sample_rate, !args.no_cache, true)?
            };
            (project, cache, ts_path, guard)
        }
        None => {
            let project = Arc::new(Project::empty(sample_rate));
            let cache = Cache::new(!args.no_cache);
            (project, cache, std::path::PathBuf::new(), None)
        }
    };

    let cache = Arc::new(Mutex::new(cache_owned));

    if project.order.len() > 0 {
        let _s = Silence::new();
        let mut prewarm = Loader::with_shared_cache(Arc::clone(&project), Arc::clone(&cache));
        let _ = prewarm.load_due(i64::MAX);
    }

    let mut queue = queue;
    if queue.playing.is_none() {
        if let Some(name) = song.and_then(|p| p.file_stem()).and_then(|s| s.to_str()) {
            let row = queue.cursor.row.min(queue.lines.len().saturating_sub(1));
            if queue.lines.get(row).map_or(false, |l| l.is_blank) {
                let text: String = name.chars().take(TEXT_W as usize).collect();
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

    let lookahead = sample_rate as i64 * 2;
    {
        let loader = Loader::with_shared_cache(Arc::clone(&project), Arc::clone(&cache));
        let tp = Arc::clone(&transport);
        let slot = reload_slot.clone();
        std::thread::spawn(move || worker(loader, tx, tp, lookahead, slot, total, wrap_tx));
    }

    let mut engine = Engine::new(&project);

    // --- Logging: session setup phases ---
    crate::log_event!("play.session.init_started");

    install_vst_hosts(&mut engine, &project);

    crate::log_event!("play.session.init_done");
    // -------------------------------------

    install_sf2_engines(&mut engine, &project, args.sf2_override.as_deref());
    install_master_fx(&mut engine, &project, args.no_master_fx);
    install_shims(&mut engine, &project, find_share_dir().as_deref());
    crate::player::inserts::install_chains(&mut engine, &project, sample_rate);

    let tp = Arc::clone(&transport);
    let mut left = vec![0f32; 4096];
    let mut right = vec![0f32; 4096];
    let mut pending_wrap: Option<Msg> = None;
    let mut cur_epoch = 0u64;

    let stream = device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _| {
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

            tp.done.store(false, Ordering::Release);

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
            // Always call render. The engine handles the tail logic internally
            // if speed == 0.
            engine.render(&mut l[head..], &mut r[head..], speed);

            if engine.pos <= 0 && speed < 0 {
                tp.speed.store(0, Ordering::Release);
            }
            tp.active_voices
            .store(engine.active_count() as i64, Ordering::Relaxed);
            for i in 0..frames {
                let o = i * channels;
                data[o] = soft_clip(l[i]);
                if channels >= 2 {
                    data[o + 1] = soft_clip(r[i]);
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

    let _notes: Vec<String> = project
    .requires
    .as_ref()
    .map(|r| crate::deps::requires::report_lines(r))
    .unwrap_or_default();

    // ── SYSTEM MEDIA CONTROLS (MPRIS) ──
    let (media_tx, media_rx) = channel();
    let tp_media = Arc::clone(&transport);
    let title = project.title.clone();
    let total_secs = project.total_secs();
    let thread_sr = sample_rate;

    std::thread::spawn(move || {
        use souvlaki::{MediaControls, PlatformConfig, MediaMetadata, MediaPlayback, MediaPosition};
        use std::time::Duration;
        use std::sync::atomic::Ordering;

        let config = PlatformConfig {
            dbus_name: "circolate",
            display_name: "Circolate",
            hwnd: None,
        };

        let mut controls = match MediaControls::new(config) {
            Ok(c) => c,
                       Err(_) => return, // Gracefully fail if OS doesn't support it
        };

        let _ = controls.attach(move |event| {
            let _ = media_tx.send(event);
        });

        let _ = controls.set_metadata(MediaMetadata {
            title: Some(&title),
                                      duration: Some(Duration::from_secs_f64(total_secs)),
                                      ..Default::default()
        });

        let mut last_pos = 0.0;
        let mut last_speed = -1;

        loop {
            if tp_media.stop.load(Ordering::Relaxed) {
                break; // Session ended, drop the MPRIS handle
            }

            let speed = tp_media.speed.load(Ordering::Relaxed);
            let pos = tp_media.position.load(Ordering::Relaxed).max(0) as f64 / thread_sr as f64;
            let progress = Some(MediaPosition(Duration::from_secs_f64(pos)));

            // Souvlaki 0.7 takes progress inside the Playback enum
            let status = if speed == 0 {
                MediaPlayback::Paused { progress }
            } else {
                MediaPlayback::Playing { progress }
            };

            // Update the OS if speed changed, or if we've moved significantly
            if speed != last_speed || (pos - last_pos).abs() > 0.5 {
                let _ = controls.set_playback(status);
                last_speed = speed;
                last_pos = pos;
            }

            std::thread::sleep(Duration::from_millis(500));
        }
    });
    // ───────────────────────────────────

    let (outcome, _queue) = tui::run_against(
        term,
        Arc::clone(&transport),
                                             Arc::clone(&project),
                                             sample_rate,
                                             queue,
                                             settings,
                                             media_rx,
    )?;
    transport.stop.store(true, Ordering::Relaxed);
    drop(stream);

    Ok(outcome)
}
