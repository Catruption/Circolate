//! Terminal lifecycle, event loop, key and mouse handling.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    cursor::Show,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::mixer::{mute, part_volume, volume};
use crate::player::{Project, Transport};
use crate::tui::config::Settings;
use crate::tui::shared::hit::{
    dispatch, hit_from_horizontal, hit_from_notch, hit_from_queue, hit_from_spiral,
    hit_from_vertical, HitAction,
};
use crate::tui::state::{QueueState, RenderState};
use crate::tui::views;

const TICK_MS: u64 = 50;
const REWIND_COOLDOWN_MS: u64 = 1000;
const SPEED_STEPS: &[i64] = &[1, 4, 16];
const HOLD_WINDOW_MS: u128 = 250;
const DOUBLE_CLICK_MS: u128 = 350;
const PERF_POLL_MS: u128 = 1000;

#[derive(Debug, Clone)]
pub enum LoopOutcome {
    Quit,
    Home,
    /// Load another track.  `flags` are the arguments chosen in the idle
    /// menu for PLAY; normal queue advancement supplies none.
    Next { path: String, flags: Vec<String> },
}

macro_rules! tui_dbg {
    ($($arg:tt)*) => {
        crate::log_event!("tui.debug", body: format!($($arg)*))
    };
}

// ── terminal lifecycle ─────────────────────────────────────────────────

// Use /dev/tty directly so we can silence fd 1 (stdout) without breaking the TUI.
pub type TuiBackend = CrosstermBackend<std::fs::File>;

pub fn setup() -> Result<Terminal<TuiBackend>> {
    enable_raw_mode()?;

    let out = std::fs::OpenOptions::new()
    .read(true)
    .write(true)
    .open("/dev/tty")?;

    execute!(out.try_clone()?, EnterAlternateScreen, EnableMouseCapture)?;

    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, handle_signal as *const () as libc::sighandler_t);
    }

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crate::tui::logs::uninstall();
        let _ = disable_raw_mode();
        if let Ok(mut out) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture, Show);
        }
        prev_hook(info);
    }));
    crate::tui::logs::install();

    if std::env::var_os("CIRCOLATE_QUIET").is_none() {
        let mut f = (*crate::log::filter()).clone();
        let mut changed = false;
        for cat in [
            crate::log::Category::Tui,
            crate::log::Category::Player,
            // Menu actions run CLI helpers that report through Deps; show
            // their structured events in the idle log panel.
            crate::log::Category::Deps,
        ] {
            if f.get(cat) < crate::log::Level::Info {
                f.set(cat, crate::log::Level::Info);
                changed = true;
            }
        }
        if changed {
            crate::log::set_filter(f);
        }
    }

    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

#[cfg(unix)]
extern "C" fn handle_signal(sig: libc::c_int) {
    unsafe {
        crate::tui::logs::uninstall();
        libc::signal(sig, libc::SIG_IGN);
        let _ = disable_raw_mode();
        if let Ok(mut out) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture, Show);
        }
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

pub fn restore(t: &mut Terminal<TuiBackend>) -> Result<()> {
    crate::tui::logs::uninstall();
    disable_raw_mode()?;
    execute!(t.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, Show)?;
    t.show_cursor()?;
    Ok(())
}

// ── session entry points ───────────────────────────────────────────────

pub fn show_loading(
    term: &mut Terminal<TuiBackend>,
    queue: &QueueState,
    settings: &Settings,
    msg: &str,
    notes: &[String],
) -> Result<()> {
    let tp = Arc::new(Transport {
        speed: std::sync::atomic::AtomicI64::new(0),
                      seek_to: std::sync::atomic::AtomicI64::new(0),
                      position: std::sync::atomic::AtomicI64::new(0),
                      done: std::sync::atomic::AtomicBool::new(false),
                      stop: std::sync::atomic::AtomicBool::new(false),
                      active_voices: std::sync::atomic::AtomicI64::new(0),
                      loaded_lines: std::sync::atomic::AtomicI64::new(0),
                      reload_count: std::sync::atomic::AtomicI64::new(0),
                      loop_next: std::sync::atomic::AtomicBool::new(false),
                      loop_wraps: std::sync::atomic::AtomicI64::new(0),
                      stereo_width: std::sync::atomic::AtomicU32::new(1.0f32.to_bits()), // Added
    });
    let project = Arc::new(Project::empty(44100));
    let mut state = RenderState::new(queue.clone(), settings.clone());
    state.loading = Some(msg.to_string());
    state.loading_notes = notes.to_vec();
    term.draw(|f| views::draw(f, &tp, &project, 44100, &mut state))?;
    Ok(())
}

pub fn run_against(
    term: &mut Terminal<TuiBackend>,
    transport: Arc<Transport>,
    project: Arc<Project>,
    sample_rate: u32,
    queue: QueueState,
    settings: Settings,
    media_rx: std::sync::mpsc::Receiver<souvlaki::MediaControlEvent>,
) -> Result<(LoopOutcome, QueueState)> {
    let mut state = RenderState::new(queue, settings);
    let outcome = event_loop(term, &transport, &project, sample_rate, &mut state, media_rx)?;
    let queue = state.queue;
    Ok((outcome, queue))
}

// ── event loop ─────────────────────────────────────────────────────────

fn event_loop(
    term: &mut Terminal<TuiBackend>,
    tp: &Arc<Transport>,
    project: &Arc<Project>,
    sample_rate: u32,
    state: &mut RenderState,
    media_rx: std::sync::mpsc::Receiver<souvlaki::MediaControlEvent>,
) -> Result<LoopOutcome> {
    let tick = Duration::from_millis(TICK_MS);
    let mut seen_wraps = tp.loop_wraps.load(Ordering::Acquire);

    loop {
        // Background menu actions report through a slot so completion text
        // is shown on the next frame instead of being written to the TUI.
        if let Ok(mut slot) = state.menu.status_slot.lock() {
            if let Some(message) = slot.take() {
                state.menu.status = Some(message);
            }
        }
        let frame_started = std::time::Instant::now();
        let draw_started = std::time::Instant::now();
        term.draw(|f| views::draw(f, tp, project, sample_rate, state))?;
        crate::player::perf::record_frame(draw_started.elapsed().as_nanos() as u64);
        loop_tick(tp, project, state);

        // ── HANDLE SYSTEM MEDIA CONTROLS ──
        while let Ok(ev) = media_rx.try_recv() {
            use souvlaki::MediaControlEvent;
            match ev {
                MediaControlEvent::Play => {
                    if tp.speed.load(Ordering::Relaxed) == 0 {
                        tp.speed.store(1, Ordering::Release);
                    }
                }
                MediaControlEvent::Pause => {
                    tp.speed.store(0, Ordering::Release);
                }
                MediaControlEvent::Toggle => {
                    let cur = tp.speed.load(Ordering::Relaxed);
                    tp.speed.store(if cur == 0 { 1 } else { 0 }, Ordering::Release);
                }
                MediaControlEvent::Next => {
                    if let Some(next) = state.queue.advance() {
                        state.play_request = Some(next);
                    }
                }
                MediaControlEvent::Previous => {
                    tp.seek_to.store(0, Ordering::Release);
                }
                MediaControlEvent::Stop => {
                    tp.speed.store(0, Ordering::Release);
                    tp.seek_to.store(0, Ordering::Release);
                }
                MediaControlEvent::Seek(souvlaki::SeekDirection::Forward) => {
                    handle_direction(KeyCode::Right, state, tp);
                }
                MediaControlEvent::Seek(souvlaki::SeekDirection::Backward) => {
                    handle_direction(KeyCode::Left, state, tp);
                }
                _ => {}
            }
        }
        // ──────────────────────────────────

        // ── PERF POLL (once per second) ──
        // Feeds the on-demand perf widget only; logging this every second
        // would drown the idle log panel with numbers nobody reads.
        let perf_now = std::time::Instant::now();
        if perf_now.duration_since(state.perf_last_poll).as_millis() >= PERF_POLL_MS {
            state.perf_snapshot = Some(crate::player::perf::perf_snapshot());
            state.perf_last_poll = perf_now;
        }
        // ────────────────────────────────

        let wraps = tp.loop_wraps.load(Ordering::Acquire);
        while seen_wraps < wraps {
            seen_wraps += 1;
            consume_repeat(state);
        }
        tp.loop_next.store(repeat_pending(state), Ordering::Release);

        if state.play_request.is_some() {
            let req = state.play_request.take().unwrap();
            return Ok(LoopOutcome::Next { path: req, flags: Vec::new() });
        }

        let speed = tp.speed.load(Ordering::Relaxed);
        let done = tp.done.load(Ordering::Acquire);
        let cooling = state.rewind_cooldown_until
        .map_or(false, |t| t > std::time::Instant::now());

        if !done {
            state.rewind_cooldown_until = None;
        } else if speed > 0 && !cooling {
            state.rewind_cooldown_until = Some(
                std::time::Instant::now() + Duration::from_millis(REWIND_COOLDOWN_MS),
            );

            if state.queue.playing.is_some() {
                let prev = state.queue.now_playing.clone();
                match state.queue.advance() {
                    Some(next) if Some(&next) == prev.as_ref() => {
                        tp.seek_to.store(0, Ordering::Release);
                    }
                    Some(next) => {
                        return Ok(LoopOutcome::Next { path: next, flags: Vec::new() });
                    }
                    None => stop_at_start(tp),
                }
            } else {
                stop_at_start(tp);
            }
        }

        if event::poll(tick.saturating_sub(frame_started.elapsed()))? {
            for event_idx in 0..64 {
                if event_idx > 0 && !event::poll(Duration::ZERO)? {
                    break;
                }
                match event::read()? {
                    Event::Key(k) => {
                        let is_ctrl_c = k.code == KeyCode::Char('c')
                        && k.modifiers.contains(KeyModifiers::CONTROL);

                        // ── ANALYSIS MODAL ──
                        // While editing a path, every character belongs to
                        // the editor.  In particular `a` must not open the
                        // analysis overlay and swallow part of the path.
                        if k.kind != KeyEventKind::Release
                        && state.menu.edit_mode.is_none()
                        {
                            if state.player.show_analysis {
                                if k.code == KeyCode::Esc
                                    || k.code == KeyCode::Char('A')
                                    || k.code == KeyCode::Char('a')
                                    || k.code == KeyCode::Char('q')
                                    {
                                        state.player.show_analysis = false;
                                        continue;
                                    }
                            } else if (k.code == KeyCode::Char('A') || k.code == KeyCode::Char('a'))
                                && !state.queue.focused
                                {
                                    state.player.show_analysis = true;
                                    state.player.show_help = false;
                                    state.player.show_config = false;
                                    state.player.show_perf = false;
                                    continue;
                                }
                        }
                        // ────────────────────

                        if project.voice_count == 0 {
                            if state.queue.focused
                                && state.menu.edit_mode.is_none()
                                && k.kind != KeyEventKind::Release
                                {
                                    handle_queue_key(k.code, k.modifiers, state);
                                    continue;
                                }

                                if k.kind == KeyEventKind::Press {
                                    if state.menu.edit_mode.is_some() {
                                        match k.code {
                                            KeyCode::Esc | KeyCode::Enter => {
                                                let edit_idx = state.menu.edit_mode.unwrap();
                                                let value = state.menu.directories[edit_idx].trim().to_string();
                                                if state.menu.edit_is_new && value.is_empty() {
                                                    state.menu.directories.pop();
                                                    state.menu.directory_kinds.pop();
                                                } else {
                                                    state.menu.directories[edit_idx] = value;
                                                    match state.menu.save_directories() {
                                                        Ok(()) => {
                                                            state.menu.status = Some("user root saved".into());
                                                            crate::log_event!("tui.menu.directories_saved");
                                                        }
                                                        Err(e) => {
                                                            state.menu.status = Some(format!("save failed: {e:#}"));
                                                            crate::log_event!("tui.menu.directories_failed", err: format!("{:#}", e));
                                                        }
                                                    }
                                                }
                                                state.menu.finish_edit();
                                                state.queue.focused = false;
                                            }
                                            KeyCode::Up => state.menu.move_edit_row(-1),
                                            KeyCode::Down => state.menu.move_edit_row(1),
                                            KeyCode::Left => state.menu.move_edit_cursor(-1),
                                            KeyCode::Right => state.menu.move_edit_cursor(1),
                                            KeyCode::Home => state.menu.edit_home(),
                                            KeyCode::End => state.menu.edit_end(),
                                            KeyCode::Char(c) => state.menu.insert_edit_char(c),
                                            KeyCode::Backspace => state.menu.backspace_edit(),
                                            KeyCode::Delete => state.menu.delete_edit(),
                                            _ => {}
                                        }
                                    } else if state.menu.submenu.is_some() {
                                        match k.code {
                                            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => {
                                                state.menu.close_submenu();
                                                state.menu.status = None;
                                            }
                                            KeyCode::Enter => {
                                                state.menu.begin_edit(0, false);
                                                state.queue.focused = false;
                                            }
                                            KeyCode::PageDown | KeyCode::Char(']') => {
                                                state.menu.system_scroll = state.menu.system_scroll.saturating_add(1);
                                            }
                                            KeyCode::PageUp | KeyCode::Char('[') => {
                                                state.menu.system_scroll = state.menu.system_scroll.saturating_sub(1);
                                            }
                                            _ => {}
                                        }
                                    } else {
                                        match k.code {
                                            KeyCode::Char('q') => return Ok(LoopOutcome::Quit),
                                            KeyCode::Up => {
                                                if state.menu.selected_index > 0 { state.menu.selected_index -= 1; }
                                            }
                                            KeyCode::Down => {
                                                if state.menu.selected_index + 1 < crate::tui::state::MENU_ITEMS {
                                                    state.menu.selected_index += 1;
                                                }
                                            }
                                            KeyCode::Left => state.menu.cycle_arg(state.menu.selected_index, -1),
                                            KeyCode::Right => state.menu.cycle_arg(state.menu.selected_index, 1),
                                            KeyCode::Char('r') => {
                                                state.menu.reload_directories();
                                                state.menu.status = Some("user root reloaded".into());
                                            }
                                            KeyCode::PageDown | KeyCode::Char(']') => {
                                                state.menu.system_scroll = state.menu.system_scroll.saturating_add(1);
                                            }
                                            KeyCode::PageUp | KeyCode::Char('[') => {
                                                state.menu.system_scroll = state.menu.system_scroll.saturating_sub(1);
                                            }
                                            KeyCode::Enter => {
                                                match state.menu.selected_index {
                                                    0 => {
                                                        if let Some(p) = crate::cli::pick::pick_file(
                                                            &crate::paths::user_halotexts(),
                                                            crate::cli::pick::FILTER_HALOTEXT,
                                                            "Open Halotext",
                                                        ) {
                                                            let flags = state.menu.selected_flags(0);
                                                            return Ok(LoopOutcome::Next {
                                                                path: p.to_string_lossy().into_owned(),
                                                                flags,
                                                            });
                                                        }
                                                    }
                                                    1 => {
                                                        if let Some(p) = crate::cli::pick::pick_file(
                                                            &crate::paths::user_halotexts(),
                                                            crate::cli::pick::FILTER_HALOTEXT,
                                                            "Dump Halotext",
                                                        ) {
                                                            let path_str = p.to_string_lossy().into_owned();
                                                            let mut args = vec![path_str.clone()];
                                                            args.extend(state.menu.selected_flags(1));
                                                            crate::log_event!("tui.menu.dump_started", path: path_str.clone());
                                                            std::thread::spawn(move || {
                                                                let result = crate::cli::dump::run(&args);
                                                                match result {
                                                                    Ok(_) => crate::log_event!("tui.menu.dump_ok", path: path_str),
                                                                    Err(e) => crate::log_event!("tui.menu.dump_failed", err: format!("{:#}", e)),
                                                                }
                                                            });
                                                        }
                                                    }
                                                    2 => {
                                                        if let Some(p) = crate::cli::pick::pick_file(
                                                            &crate::paths::export_dir(),
                                                            crate::cli::pick::FILTER_SOURCE,
                                                            "Export Source",
                                                        ) {
                                                            let path_str = p.to_string_lossy().into_owned();
                                                            let mut args = vec![path_str.clone()];
                                                            args.extend(state.menu.selected_flags(2));
                                                            crate::log_event!("tui.menu.export_started", path: path_str.clone());
                                                            std::thread::spawn(move || {
                                                                let result = crate::cli::export::run(&args);
                                                                match result {
                                                                    Ok(_) => crate::log_event!("tui.menu.export_ok", path: path_str),
                                                                    Err(e) => crate::log_event!("tui.menu.export_failed", err: format!("{:#}", e)),
                                                                }
                                                            });
                                                        }
                                                    }
                                                    3 => {
                                                        state.menu.status = Some("installing directories...".into());
                                                        crate::log_event!("tui.menu.install_started");
                                                        let mut flags = vec!["--log-only".to_string()];
                                                        flags.extend(state.menu.selected_flags(3));
                                                        let slot = std::sync::Arc::clone(&state.menu.status_slot);
                                                        std::thread::spawn(move || {
                                                            match crate::deps::setup::run_install(&flags) {
                                                                Ok(()) => {
                                                                    crate::log_event!("tui.menu.install_ok");
                                                                    if let Ok(mut s) = slot.lock() {
                                                                        *s = Some("directory install finished — see log".into());
                                                                    }
                                                                }
                                                                Err(e) => {
                                                                    crate::log_event!("tui.menu.install_failed", err: format!("{:#}", e));
                                                                    if let Ok(mut s) = slot.lock() {
                                                                        *s = Some(format!("install failed: {e:#}"));
                                                                    }
                                                                }
                                                            }
                                                        });
                                                    }
                                                    4 => {
                                                        state.menu.status = Some("scanning dependencies...".into());
                                                        crate::log_event!("tui.menu.doctor_started");
                                                        let mut args = vec!["--log-only".to_string()];
                                                        args.extend(state.menu.selected_flags(4));
                                                        let slot = std::sync::Arc::clone(&state.menu.status_slot);
                                                        std::thread::spawn(move || {
                                                            match crate::deps::doctor::run(&args) {
                                                                Ok(()) => {
                                                                    crate::log_event!("tui.menu.doctor_ok");
                                                                    if let Ok(mut s) = slot.lock() {
                                                                        *s = Some("dependency scan finished — see log".into());
                                                                    }
                                                                }
                                                                Err(e) => {
                                                                    crate::log_event!("tui.menu.doctor_failed", err: format!("{:#}", e));
                                                                    if let Ok(mut s) = slot.lock() {
                                                                        *s = Some(format!("scan failed: {e:#}"));
                                                                    }
                                                                }
                                                            }
                                                        });
                                                    }
                                                    5 => {
                                                        state.menu.status = Some("fetching plugins...".into());
                                                        crate::log_event!("tui.menu.fetch_started");
                                                        let flags = state.menu.selected_flags(5);
                                                        let slot = std::sync::Arc::clone(&state.menu.status_slot);
                                                        std::thread::spawn(move || {
                                                            match crate::deps::fetch::run(&flags) {
                                                                Ok(()) => {
                                                                    crate::log_event!("tui.menu.fetch_ok");
                                                                    if let Ok(mut s) = slot.lock() {
                                                                        *s = Some("plugin fetch finished — see log".into());
                                                                    }
                                                                }
                                                                Err(e) => {
                                                                    crate::log_event!("tui.menu.fetch_failed", err: format!("{:#}", e));
                                                                    if let Ok(mut s) = slot.lock() {
                                                                        *s = Some(format!("fetch failed: {e:#}"));
                                                                    }
                                                                }
                                                            }
                                                        });
                                                    }
                                                    6 => {
                                                        state.menu.open_directories();
                                                        state.queue.focused = false;
                                                    }
                                                    _ => {}
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                continue;
                        }

                        let typing = state.queue.focused
                        && !state.player.show_help
                        && !state.player.show_config
                        && !state.player.show_mixer
                        && !state.player.show_perf
                        && !is_ctrl_c;

                        if !typing
                            && !state.player.show_mixer
                            && !state.player.show_config
                            && k.kind == KeyEventKind::Press
                            && matches!(k.code, KeyCode::Left | KeyCode::Right)
                            {
                                handle_direction(k.code, state, tp);
                            }

                            if k.kind != KeyEventKind::Release {
                                if state.player.show_config {
                                    let action = crate::tui::widgets::notch::config::handle_key(
                                        k.code,
                                        k.modifiers,
                                        &mut state.config_ui,
                                        &mut state.settings,
                                    );
                                    if matches!(
                                        action,
                                        crate::tui::widgets::notch::config::Action::Close
                                    ) {
                                        state.player.show_config = false;
                                    }
                                } else if state.player.show_mixer {
                                    match k.code {
                                        KeyCode::Esc | KeyCode::Char('q') => state.player.show_mixer = false,
                                        KeyCode::Left => {
                                            let cur = f32::from_bits(tp.stereo_width.load(Ordering::Relaxed));
                                            let next = (cur - 0.05).max(0.0);
                                            tp.stereo_width.store(next.to_bits(), Ordering::Release);
                                        }
                                        KeyCode::Right => {
                                            let cur = f32::from_bits(tp.stereo_width.load(Ordering::Relaxed));
                                            let next = (cur + 0.05).min(3.0);
                                            tp.stereo_width.store(next.to_bits(), Ordering::Release);
                                        }
                                        _ => {}
                                    }
                                } else if typing {
                                    handle_queue_key(k.code, k.modifiers, state);
                                } else if state.player.show_perf
                                    && (k.code == KeyCode::Esc
                                    || k.code == KeyCode::Char('q')
                                    || is_ctrl_c)
                                    {
                                        state.player.show_perf = false;
                                    } else if state.player.show_help
                                        && (k.code == KeyCode::Esc
                                        || k.code == KeyCode::Char('q')
                                        || is_ctrl_c)
                                        {
                                            state.player.show_help = false;
                                        } else if is_ctrl_c {
                                            return Ok(LoopOutcome::Quit);
                                        } else if k.code == KeyCode::Char('q') || k.code == KeyCode::Esc {
                                            return Ok(LoopOutcome::Home);
                                        } else {
                                            handle_key(k.code, k.modifiers, tp, project, state);
                                        }
                            }
                    }
                    Event::Mouse(m) => handle_mouse(m, tp, project, state),
                    other => {
                        tui_dbg!("[event] {:?}", other);
                    }
                }
            }
        }
    }
}

fn repeat_pending(state: &RenderState) -> bool {
    state.queue.playing
    .and_then(|cur| state.queue.lines.get(cur))
    .map_or(false, |l| l.count > 0)
}

fn consume_repeat(state: &mut RenderState) {
    if repeat_pending(state) {
        let _ = state.queue.advance();
    }
}

fn stop_at_start(tp: &Arc<Transport>) {
    tp.speed.store(0, Ordering::Release);
    tp.seek_to.store(0, Ordering::Release);
    tp.done.store(false, Ordering::Release);
}

// ── loop enforcement ───────────────────────────────────────────────────

fn loop_tick(tp: &Arc<Transport>, p: &Project, state: &mut RenderState) {
    if !state.player.loop_enabled || state.player.loop_measures.is_empty() {
        return;
    }
    if tp.speed.load(Ordering::Relaxed) <= 0 { return; }
    let pos = tp.position.load(Ordering::Relaxed);
    let Some(cur) = p.current_line(pos) else { return };

    if state.player.loop_measures.contains(&cur) {
        state.player.last_loop_check_measure = None;
        return;
    }
    if state.player.last_loop_check_measure == Some(cur) { return; }
    state.player.last_loop_check_measure = Some(cur);

    let next = state.player.loop_measures.iter()
    .filter(|&&m| m > cur).min().copied()
    .unwrap_or_else(|| *state.player.loop_measures.iter().min().unwrap());
    tp.seek_to.store(p.line_start_sample(next), Ordering::Release);
}

// ── key handling ───────────────────────────────────────────────────────

fn handle_direction(code: KeyCode, state: &mut RenderState, tp: &Arc<Transport>) {
    let now = std::time::Instant::now();
    let is_hold = matches!(
        state.last_dir_press,
        Some((last, t)) if last == code && now.duration_since(t).as_millis() < HOLD_WINDOW_MS
    );
    state.last_dir_press = Some((code, now));

    let cur = tp.speed.load(Ordering::Relaxed);
    let want_sign: i64 = if code == KeyCode::Left { -1 } else { 1 };

    let next = if is_hold {
        let mag = if cur.signum() == want_sign { cur.unsigned_abs() as i64 } else { 0 };
        SPEED_STEPS.iter().copied().find(|s| *s > mag)
        .unwrap_or(*SPEED_STEPS.last().unwrap()) * want_sign
    } else if cur.signum() == want_sign {
        0
    } else {
        want_sign
    };
    if next != cur {
        tp.speed.store(next, Ordering::Release);
        crate::log_event!("player.speed.changed", speed: next);
    }
}

fn handle_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    tp: &Arc<Transport>,
    p: &Project,
    state: &mut RenderState,
) {
    tui_dbg!("[key] {:?} mods={:?}", code, modifiers);
    let _ = modifiers;
    match code {
        KeyCode::Home => {
            tp.seek_to.store(0, Ordering::Release);
            tp.speed.store(0, Ordering::Release);
            state.player.last_loop_check_measure = None;
        }
        KeyCode::Char('?') => {
            state.player.show_help = !state.player.show_help;
            if state.player.show_help {
                state.player.show_perf = false;
            }
        }
        KeyCode::Char('P') => {
            state.player.show_perf = !state.player.show_perf;
            if state.player.show_perf {
                state.player.show_help = false;
                state.player.show_config = false;
                state.player.show_analysis = false;
            }
        }
        KeyCode::Char('x') => {
            state.player.show_mixer = !state.player.show_mixer;
            if state.player.show_mixer {
                state.player.show_help = false;
                state.player.show_config = false;
                state.player.show_analysis = false;
                state.player.show_perf = false;
            }
        }
        KeyCode::Char('L') => {
            state.player.loop_enabled = !state.player.loop_enabled;
            state.player.last_loop_check_measure = None;
        }
        KeyCode::Char('c') => mute::clear_all(),
        KeyCode::Char('+') | KeyCode::Char('=') => volume_step(state, 0.05),
        KeyCode::Char('-') | KeyCode::Char('_') => volume_step(state, -0.05),
        KeyCode::Up => volume_step(state, 0.05),
        KeyCode::Down => volume_step(state, -0.05),
        KeyCode::Char('M') => {
            volume::toggle_mute();
            state.volume_last_poll = std::time::Instant::now()
            - std::time::Duration::from_secs(60);
        }
        KeyCode::Char('[') => {
            let pos = tp.position.load(Ordering::Relaxed);
            if let Some(cur) = p.current_line(pos) {
                if cur > 0 {
                    tp.seek_to.store(p.line_start_sample(cur - 1), Ordering::Release);
                    state.player.last_loop_check_measure = None;
                }
            }
        }
        KeyCode::Char(']') => {
            let pos = tp.position.load(Ordering::Relaxed);
            if let Some(cur) = p.current_line(pos) {
                if cur + 1 < p.line_count() {
                    tp.seek_to.store(p.line_start_sample(cur + 1), Ordering::Release);
                    state.player.last_loop_check_measure = None;
                }
            }
        }
        KeyCode::Char('h') => solo_step(state, -1),
        KeyCode::Char('l') => solo_step(state, 1),
        KeyCode::Char('m') => {
            if let Some(f) = mute::soloed() {
                mute::toggle_part_mute(&f);
            }
        }
        _ => {}
    }
}

fn handle_queue_key(code: KeyCode, modifiers: KeyModifiers, state: &mut RenderState) {
    let plain = !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match code {
        KeyCode::Esc => state.queue.focused = false,
        KeyCode::Enter => {
            if let Some(name) = state.queue.play_current() {
                state.play_request = Some(name);
                state.queue.focused = false;
            }
        }
        KeyCode::Char(c) if plain => state.queue.insert_char(c),
        KeyCode::Backspace => state.queue.backspace(),
        KeyCode::Delete => state.queue.delete_char(),
        KeyCode::Left => state.queue.cursor_left(),
        KeyCode::Right => state.queue.cursor_right(),
        KeyCode::Up => state.queue.cursor_up(),
        KeyCode::Down => state.queue.cursor_down(),
        KeyCode::Home => state.queue.cursor_home(),
        KeyCode::End => state.queue.cursor_end(),
        KeyCode::PageUp => state.queue.cycle_loop(1),
        KeyCode::PageDown => state.queue.cycle_loop(-1),
        _ => {}
    }
}

fn solo_step(state: &RenderState, dir: i32) {
    let order = &state.column_order;
    if order.is_empty() { return; }
    let cur = mute::soloed();
    let cur_idx = cur.as_ref().and_then(|f| order.iter().position(|p| p == f));
    let next_idx = match cur_idx {
        Some(i) => (i as i32 + dir).clamp(0, order.len() as i32 - 1) as usize,
        None => 0,
    };
    mute::set_solo(Some(&order[next_idx]));
}

fn volume_step(state: &mut RenderState, delta: f64) {
    volume::step(delta);
    state.volume_last_poll = std::time::Instant::now()
    - std::time::Duration::from_secs(60);
}

// ── mouse ──────────────────────────────────────────────────────────────

fn handle_mouse(m: MouseEvent, tp: &Arc<Transport>, p: &Project, state: &mut RenderState) {
    tui_dbg!(
        "[mouse] kind={:?} col={} row={}",
        m.kind, m.column, m.row
    );

    match m.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let dir = if matches!(m.kind, MouseEventKind::ScrollUp) { 1 } else { -1 };
            let action = hit_from_vertical(state, m)
            .or_else(|| hit_from_horizontal(state, m));
            match action {
                Some(HitAction::PartLabel { path }) => {
                    let new_vol = part_volume::step(&path, dir);
                    tui_dbg!(
                        "[scroll] path={:?} dir={} new_vol={:.3}",
                        path, dir, new_vol
                    );
                }
                Some(_) => {
                    tui_dbg!("[scroll] hit resolved to non-header action — ignored");
                }
                None => {
                    tui_dbg!("[scroll] no hit — pointer not on any clickable row");
                }
            }
            return;
        }
        _ => {}
    }

    let button = match m.kind {
        MouseEventKind::Down(b) | MouseEventKind::Drag(b) => b,
        _ => return,
    };

    if let Some(action) = hit_from_queue(state, m) {
        if matches!(button, MouseButton::Left) {
            state.queue.focused = true;
        }
        match action {
            HitAction::QueueRewind => {
                if matches!(button, MouseButton::Left) {
                    handle_direction(KeyCode::Left, state, tp);
                }
            }
            HitAction::QueuePlay => {
                if matches!(button, MouseButton::Left) {
                    handle_direction(KeyCode::Right, state, tp);
                }
            }
            HitAction::QueueLine { row, col } => {
                if matches!(button, MouseButton::Left) {
                    state.queue.cursor.row = row;
                    let text_len = state.queue.lines.get(row).map(|l| l.text.chars().count()).unwrap_or(0);
                    state.queue.cursor.col = col.min(text_len);
                }
            }
            HitAction::QueueGutter { row } => {
                if matches!(button, MouseButton::Left) || matches!(button, MouseButton::Right) {
                    state.queue.cursor.row = row;
                    state.queue.cycle_loop(if button == MouseButton::Right { -1 } else { 1 });
                }
            }
            _ => {}
        }
        return;
    }
    state.queue.focused = false;

    let action = hit_from_notch(state, m)
    .or_else(|| hit_from_spiral(state, m))
    .or_else(|| hit_from_vertical(state, m))
    .or_else(|| hit_from_horizontal(state, m));

    if button == MouseButton::Left && matches!(m.kind, MouseEventKind::Down(_)) {
        let now = std::time::Instant::now();
        let is_double = matches!(
            state.player.last_left_click,
            Some((t, x, y)) if x == m.column && y == m.row
            && now.duration_since(t).as_millis() < DOUBLE_CLICK_MS
        );
        let on_header = matches!(action, Some(HitAction::PartLabel { .. }));
        if is_double && on_header {
            tui_dbg!("[mouse] double-click on header — clear_all mutes");
            mute::clear_all();
            state.player.last_left_click = None;
            return;
        }
        state.player.last_left_click = Some((now, m.column, m.row));
    }

    let Some(action) = action else { return; };

    if matches!(m.kind, MouseEventKind::Drag(_)) {
        let scrub = button == MouseButton::Left
        && matches!(
            action,
            HitAction::SpiralMeasure { .. } | HitAction::Waveform { .. }
        );
        if !scrub {
            return;
        }
    }

    dispatch(action, button, tp, p, state);
}
