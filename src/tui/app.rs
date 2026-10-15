//! Terminal lifecycle, event loop, key and mouse handling.  Everything
//! that isn't drawing or state.

use std::io::{self, Stdout};
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
use ratatui::{backend::CrosstermBackend, layout::Rect, Terminal};

use crate::mixer::{mute, volume};
use crate::player::{Project, Transport};
use crate::tui::config::Settings;
use crate::tui::shared::hit::{
    dispatch, hit_from_horizontal, hit_from_notch, hit_from_spiral, hit_from_vertical,
    HitAction,
};
use crate::tui::state::{Click, QueueState, RenderState};
use crate::tui::views;

const TICK_MS: u64 = 50;
const REWIND_COOLDOWN_MS: u64 = 1000;
const SPEED_STEPS: &[i64] = &[1, 4, 16];
const HOLD_WINDOW_MS: u128 = 250;
const DOUBLE_CLICK_MS: u128 = 350;

// ── terminal lifecycle ─────────────────────────────────────────────────

pub fn setup() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;

    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, handle_signal as *const () as libc::sighandler_t);
    }

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture, Show);
        prev_hook(info);
    }));

    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

#[cfg(unix)]
extern "C" fn handle_signal(sig: libc::c_int) {
    unsafe {
        libc::signal(sig, libc::SIG_IGN);
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture, Show);
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

pub fn restore(t: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(t.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, Show)?;
    t.show_cursor()?;
    Ok(())
}

// ── session entry points ───────────────────────────────────────────────

pub fn show_loading(
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    queue: &QueueState,
    settings: &Settings,
    msg: &str,
    notes: &[String],
) -> Result<()> {
    use std::sync::atomic::{AtomicBool, AtomicI64};

    let tp = Arc::new(Transport {
        speed: AtomicI64::new(0),
        seek_to: AtomicI64::new(0),
        position: AtomicI64::new(0),
        done: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        active_voices: AtomicI64::new(0),
        loaded_lines: AtomicI64::new(0),
        reload_count: AtomicI64::new(0),
        loop_next: AtomicBool::new(false),
        loop_wraps: AtomicI64::new(0),
    });
    let project = Arc::new(Project::empty(44100));
    let mut state = RenderState::new(queue.clone(), settings.clone());
    state.loading = Some(msg.to_string());
    state.loading_notes = notes.to_vec();
    term.draw(|f| views::draw(f, &tp, &project, 44100, &mut state))?;
    Ok(())
}

pub fn run_against(
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    transport: Arc<Transport>,
    project: Arc<Project>,
    sample_rate: u32,
    queue: QueueState,
    settings: Settings,
) -> Result<(Option<String>, QueueState)> {
    let mut state = RenderState::new(queue, settings);

    if project.voice_count == 0 {
        state.queue.focused = true;
    }

    event_loop(term, &transport, &project, sample_rate, &mut state)?;
    let request = state.play_request.take();
    let queue = state.queue;
    Ok((request, queue))
}

// ── event loop ─────────────────────────────────────────────────────────

fn event_loop(
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    tp: &Arc<Transport>,
    project: &Arc<Project>,
    sample_rate: u32,
    state: &mut RenderState,
) -> Result<()> {
    let tick = Duration::from_millis(TICK_MS);
    let mut seen_wraps = tp.loop_wraps.load(Ordering::Acquire);

    loop {
        term.draw(|f| views::draw(f, tp, project, sample_rate, state))?;
        loop_tick(tp, project, state);

        let wraps = tp.loop_wraps.load(Ordering::Acquire);
        while seen_wraps < wraps {
            seen_wraps += 1;
            consume_repeat(state);
        }
        tp.loop_next.store(repeat_pending(state), Ordering::Release);

        if state.play_request.is_some() { break; }

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
                        state.play_request = Some(next);
                        break;
                    }
                    None => stop_at_start(tp),
                }
            } else {
                stop_at_start(tp);
            }
        }

        if event::poll(tick)? {
            match event::read()? {
                Event::Key(k) => {
                    let is_ctrl_c = k.code == KeyCode::Char('c')
                        && k.modifiers.contains(KeyModifiers::CONTROL);
                    let typing = state.queue.focused
                        && !state.player.show_help
                        && !is_ctrl_c;
                    if !typing
                        && k.kind == KeyEventKind::Press
                        && matches!(k.code, KeyCode::Left | KeyCode::Right)
                    {
                        handle_direction(k.code, state, tp);
                    }
                    if k.kind != KeyEventKind::Release {
                        if typing {
                            handle_queue_key(k.code, k.modifiers, state);
                        } else if state.player.show_help
                            && (k.code == KeyCode::Esc
                                || k.code == KeyCode::Char('q')
                                || is_ctrl_c)
                        {
                            state.player.show_help = false;
                        } else if k.code == KeyCode::Char('q')
                            || k.code == KeyCode::Esc
                            || is_ctrl_c
                        {
                            break;
                        } else {
                            handle_key(k.code, k.modifiers, tp, project, state);
                        }
                    }
                }
                Event::Mouse(m) => handle_mouse(m, tp, project, state),
                _ => {}
            }
        }

        if state.play_request.is_some() { break; }
    }
    Ok(())
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

    if is_hold {
        let mag = if cur.signum() == want_sign { cur.unsigned_abs() as i64 } else { 0 };
        let next = SPEED_STEPS.iter().copied().find(|s| *s > mag)
            .unwrap_or(*SPEED_STEPS.last().unwrap());
        tp.speed.store(want_sign * next, Ordering::Release);
    } else {
        let already = cur.signum() == want_sign;
        tp.speed.store(if already { 0 } else { want_sign }, Ordering::Release);
    }
}

fn handle_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    tp: &Arc<Transport>,
    p: &Project,
    state: &mut RenderState,
) {
    let _ = modifiers;
    match code {
        KeyCode::Home => {
            tp.seek_to.store(0, Ordering::Release);
            tp.speed.store(0, Ordering::Release);
            state.player.last_loop_check_measure = None;
        }
        KeyCode::Char('?') => state.player.show_help = !state.player.show_help,
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
    let button = match m.kind {
        MouseEventKind::Down(b) | MouseEventKind::Drag(b) => b,
        _ => return,
    };

    let has = |r: Rect| {
        m.column >= r.x && m.column < r.x + r.width && m.row >= r.y && m.row < r.y + r.height
    };
    let in_box = state.queue_placed
        .map_or(false, |q| has(q.notch) && !has(q.spiral));
    if !in_box && matches!(m.kind, MouseEventKind::Down(_)) {
        state.queue.focused = false;
    }
    if in_box {
        if let (MouseEventKind::Down(b), Some(q)) = (m.kind, state.queue_placed) {
            match state.queue.click(&q, m.column, m.row, b == MouseButton::Right) {
                Click::Rewind => handle_direction(KeyCode::Left, state, tp),
                Click::Play => handle_direction(KeyCode::Right, state, tp),
                Click::None => {}
            }
        }
        return;
    }

    if button == MouseButton::Left && matches!(m.kind, MouseEventKind::Down(_)) {
        let now = std::time::Instant::now();
        let is_double = matches!(
            state.player.last_left_click,
            Some((t, x, y)) if x == m.column && y == m.row
                && now.duration_since(t).as_millis() < DOUBLE_CLICK_MS
        );
        if is_double {
            mute::clear_all();
            state.player.last_left_click = None;
            return;
        }
        state.player.last_left_click = Some((now, m.column, m.row));
    }

        let action = hit_from_notch(state, m)
        .or_else(|| hit_from_spiral(state, m))
        .or_else(|| hit_from_vertical(state, m))
        .or_else(|| hit_from_horizontal(state, m));

    let Some(action) = action else { return; };

    // Drag should only drive continuous seek: a left-button press held
    // while moving across the spiral or a waveform scrubs the playhead.
    // Everything else (toggles, one-shot commands) fires on press only —
    // otherwise a 1px mouse jitter re-triggers the same action twice and
    // the user sees the state flip back to where it started.
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