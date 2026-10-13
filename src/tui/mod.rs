//! Ratatui front-end for live playback.

pub mod cover;
pub mod draw;
pub mod envelope;
pub mod mute;
pub mod notes;
pub mod scope;
pub mod spiral;
pub mod style;
pub mod theme;

pub use scope::scope_push;

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
use ratatui::{
    backend::CrosstermBackend,
    layout::Rect,
    Terminal,
};
use std::collections::{HashMap, HashSet};
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::player::{Project, Transport};
use crate::tui::cover::Cover;
use crate::tui::draw::draw;
use crate::tui::spiral::SpiralState;

#[allow(dead_code)]
const SEEK_STEP_SECS: f64 = 5.0;
const TICK_MS: u64 = 50;
const SPEED_STEPS: &[i64] = &[1, 4, 16];
const HOLD_WINDOW_MS: u128 = 250;
const DOUBLE_CLICK_MS: u128 = 350;

// ── per-column sample playhead ─────────────────────────────────────────

pub struct ColumnPlayhead {
    pub note_id: Option<(String, f64, i64)>,
    pub position: f64,
    pub last_audio_pos: i64,
}

impl Default for ColumnPlayhead {
    fn default() -> Self {
        Self { note_id: None, position: 0.0, last_audio_pos: 0 }
    }
}

// ── click targets ──────────────────────────────────────────────────────

pub struct TrackerColumnHit {
    pub path: String,
    pub x_start: u16,
    pub width: u16,
    pub header_y: u16,
    pub rows: Vec<Option<(usize, i64)>>,
}

pub struct TrackerHit {
    pub inner: Rect,
    pub columns: Vec<TrackerColumnHit>,
}

// ── player UI state ────────────────────────────────────────────────────

pub struct PlayerUi {
    pub loop_measures: HashSet<usize>,
    pub loop_enabled: bool,
    pub last_loop_check_measure: Option<usize>,
    pub focused_part: Option<String>,
    pub show_help: bool,
    pub last_left_click: Option<(std::time::Instant, u16, u16)>,
    pub verb_mode: bool,
    /// Last `reload_count` value the TUI has seen.  When the transport's
    /// counter ticks past this, the flash timer starts.
    pub last_reload_count: i64,
    /// Set when a reload is freshly observed.  The footer flashes
    /// "reloaded" until this instant passes.
    pub reload_flash_until: Option<std::time::Instant>,
}

impl Default for PlayerUi {
    fn default() -> Self {
        Self {
            loop_measures: HashSet::new(),
            loop_enabled: false,
            last_loop_check_measure: None,
            focused_part: None,
            show_help: false,
            last_left_click: None,
            verb_mode: false,
            last_reload_count: 0,
            reload_flash_until: None,
        }
    }
}

// ── render state ───────────────────────────────────────────────────────

pub struct RenderState {
    pub last_synth_env: Vec<Vec<(f32, f32)>>,
    pub snapshot: Vec<f32>,
    pub column_order: Vec<String>,
    pub cover: Option<Cover>,
    pub cover_source: Option<PathBuf>,
    pub cover_dims: (usize, usize),
    pub last_dir_press: Option<(KeyCode, std::time::Instant)>,
    pub spiral: Option<SpiralState>,
    pub playheads: HashMap<String, ColumnPlayhead>,
    pub sample_env_cache: HashMap<(String, usize), Vec<(f32, f32)>>,
    pub player: PlayerUi,
    pub tracker_hit: Option<TrackerHit>,
}

impl RenderState {
    fn new() -> Self {
        Self {
            last_synth_env: Vec::new(),
            snapshot: Vec::with_capacity(scope::SCOPE_LEN),
            column_order: Vec::new(),
            cover: None,
            cover_source: None,
            cover_dims: (0, 0),
            last_dir_press: None,
            spiral: None,
            playheads: HashMap::new(),
            sample_env_cache: HashMap::new(),
            player: PlayerUi::default(),
            tracker_hit: None,
        }
    }
}

// ── entry point ────────────────────────────────────────────────────────

pub fn run(transport: Arc<Transport>, project: Arc<Project>, sample_rate: u32) -> Result<()> {
    let mut term = setup()?;
    let mut state = RenderState::new();
    let result = event_loop(&mut term, &transport, &project, sample_rate, &mut state);
    let _ = restore(&mut term);
    transport.stop.store(true, Ordering::Relaxed);
    result
}

fn setup() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;

    // Signal handlers so SIGINT/SIGTERM/SIGHUP restore the terminal
    // instead of leaving it in raw mode with the alternate screen
    // active.  Without this, `kill <pid>` or a terminal-side hangup
    // wedges the session until the user runs `reset`.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, handle_signal as *const () as libc::sighandler_t);
    }

    // Panic hook: restore the terminal before the default hook prints
    // the panic message.  Otherwise a panic in the render loop leaves
    // the terminal in raw mode and the panic text is rendered inside
    // the alternate screen, which is then lost on exit.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show,
        );
        prev_hook(info);
    }));

    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

/// Async-signal handler that restores the terminal, then re-raises the
/// signal with the default disposition so the process exits with the
/// correct status.
#[cfg(unix)]
extern "C" fn handle_signal(sig: libc::c_int) {
    unsafe {
        // Ignore further signals while we restore, so a second Ctrl-C
        // doesn't race us.
        libc::signal(sig, libc::SIG_IGN);
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show,
        );
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

fn restore(t: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(t.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, Show)?;
    t.show_cursor()?;
    Ok(())
}

fn event_loop(
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    tp: &Arc<Transport>,
    project: &Arc<Project>,
    sample_rate: u32,
    state: &mut RenderState,
) -> Result<()> {
    let tick = Duration::from_millis(TICK_MS);

    loop {
        term.draw(|f| draw(f, tp, project, sample_rate, state))?;
        loop_tick(tp, project, state);

        if event::poll(tick)? {
            match event::read()? {
                Event::Key(k) => {
                    if k.kind == KeyEventKind::Press
                        && matches!(k.code, KeyCode::Left | KeyCode::Right)
                    {
                        handle_direction(k.code, state, tp);
                    }
                    if k.kind != KeyEventKind::Release {
                        let is_ctrl_c = k.code == KeyCode::Char('c')
                            && k.modifiers.contains(KeyModifiers::CONTROL);

                        if state.player.show_help
                            && (k.code == KeyCode::Esc
                                || k.code == KeyCode::Char('q')
                                || is_ctrl_c)
                        {
                            state.player.show_help = false;
                        } else if k.code == KeyCode::Char('q')
                            || k.code == KeyCode::Esc
                            || is_ctrl_c
                        {
                            // Break out of the loop so `run()` can call
                            // `restore()`.  Do NOT call
                            // `std::process::exit` anywhere in this
                            // function — it skips the terminal cleanup
                            // and leaves the user's shell wedged in
                            // raw mode.
                            break;
                        } else {
                            handle_key(k.code, k.modifiers, tp, project, state);
                        }
                    }
                }
                Event::Mouse(m) => {
                    handle_mouse(m, tp, project, state);
                }
                _ => {}
            }
        }

        if tp.done.load(Ordering::Acquire) {
            term.draw(|f| draw(f, tp, project, sample_rate, state))?;
            std::thread::sleep(Duration::from_millis(500));
            break;
        }
    }
    Ok(())
}

/// Enforce loop playback.  Every tick, if the transport is playing
/// forward and the current measure isn't in the loop set, seek to the
/// nearest measure that is.
fn loop_tick(tp: &Arc<Transport>, p: &Project, state: &mut RenderState) {
    if !state.player.loop_enabled || state.player.loop_measures.is_empty() {
        return;
    }
    let speed = tp.speed.load(Ordering::Relaxed);
    if speed <= 0 {
        return;
    }
    let pos = tp.position.load(Ordering::Relaxed);
    let Some(cur) = p.current_line(pos) else { return };

    if state.player.loop_measures.contains(&cur) {
        state.player.last_loop_check_measure = None;
        return;
    }

    if state.player.last_loop_check_measure == Some(cur) {
        return;
    }
    state.player.last_loop_check_measure = Some(cur);

    let next = state
        .player
        .loop_measures
        .iter()
        .filter(|&&m| m > cur)
        .min()
        .copied()
        .unwrap_or_else(|| *state.player.loop_measures.iter().min().unwrap());
    let target = p.line_start_sample(next);

    if std::env::var_os("LOOP_DEBUG").is_some() {
        eprintln!(
            "loop: pos={} cur={} loop_set={:?} -> seeking measure {} (sample {})",
            pos, cur, state.player.loop_measures, next, target
        );
    }

    tp.seek_to.store(target, Ordering::Release);
}

fn handle_direction(code: KeyCode, state: &mut RenderState, tp: &Arc<Transport>) {
    let now = std::time::Instant::now();
    let is_hold = matches!(
        state.last_dir_press,
        Some((last, t))
            if last == code && now.duration_since(t).as_millis() < HOLD_WINDOW_MS
    );
    state.last_dir_press = Some((code, now));

    let cur = tp.speed.load(Ordering::Relaxed);
    let want_sign: i64 = if code == KeyCode::Left { -1 } else { 1 };

    if is_hold {
        let mag = if cur.signum() == want_sign { cur.unsigned_abs() as i64 } else { 0 };
        let next = SPEED_STEPS
            .iter()
            .copied()
            .find(|s| *s > mag)
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
    // Ctrl-C is handled by the event loop (it needs to break out of
    // the run loop so `restore()` runs).  Nothing to do here for it.
    let _ = modifiers;

    match code {
        KeyCode::Home => {
            tp.seek_to.store(0, Ordering::Release);
            tp.speed.store(0, Ordering::Release);
            state.player.last_loop_check_measure = None;
        }
        KeyCode::Char('?') => {
            state.player.show_help = !state.player.show_help;
        }
        KeyCode::Char('L') => {
            state.player.loop_enabled = !state.player.loop_enabled;
            state.player.last_loop_check_measure = None;
        }
        KeyCode::Char('c') => {
            mute::clear_all();
        }
        KeyCode::Char('v') => {
            state.player.verb_mode = !state.player.verb_mode;
        }
        KeyCode::Char('[') => {
            let pos = tp.position.load(Ordering::Relaxed);
            if let Some(cur) = p.current_line(pos) {
                if cur > 0 {
                    let target = p.line_start_sample(cur - 1);
                    tp.seek_to.store(target, Ordering::Release);
                    state.player.last_loop_check_measure = None;
                }
            }
        }
        KeyCode::Char(']') => {
            let pos = tp.position.load(Ordering::Relaxed);
            if let Some(cur) = p.current_line(pos) {
                if cur + 1 < p.line_count() {
                    let target = p.line_start_sample(cur + 1);
                    tp.seek_to.store(target, Ordering::Release);
                    state.player.last_loop_check_measure = None;
                }
            }
        }
        KeyCode::Char('h') => focus_step(state, -1),
        KeyCode::Char('l') => focus_step(state, 1),
        KeyCode::Char('m') => {
            if let Some(f) = state.player.focused_part.clone() {
                mute::toggle_part_mute(&f);
            }
        }
        KeyCode::Char('s') => {
            if let Some(f) = state.player.focused_part.clone() {
                mute::toggle_part_solo(&f);
            }
        }
        _ => {}
    }
}

fn focus_step(state: &mut RenderState, dir: i32) {
    let order = &state.column_order;
    if order.is_empty() {
        return;
    }
    let cur_idx = state
        .player
        .focused_part
        .as_ref()
        .and_then(|f| order.iter().position(|p| p == f));
    let next_idx = match cur_idx {
        Some(i) => {
            let j = i as i32 + dir;
            j.clamp(0, order.len() as i32 - 1) as usize
        }
        None => 0,
    };
    state.player.focused_part = Some(order[next_idx].clone());
}

/// One mouse event.  Splits into three regions: spiral, tracker, or
/// elsewhere.  Right-click actions are exposed through a sticky "mute
/// mode" toggled by `v`, because every modifier-key combination
/// conflicts with some window manager or terminal feature.
fn handle_mouse(m: MouseEvent, tp: &Arc<Transport>, p: &Project, state: &mut RenderState) {
    let is_down_or_drag = matches!(
        m.kind,
        MouseEventKind::Down(_) | MouseEventKind::Drag(_)
    );
    if !is_down_or_drag {
        return;
    }
    let raw_button = match m.kind {
        MouseEventKind::Down(b) | MouseEventKind::Drag(b) => b,
        _ => return,
    };

    let button = if raw_button == MouseButton::Left && state.player.verb_mode {
        MouseButton::Right
    } else {
        raw_button
    };

    if button == MouseButton::Left && matches!(m.kind, MouseEventKind::Down(_)) {
        let now = std::time::Instant::now();
        let is_double = matches!(
            state.player.last_left_click,
            Some((t, x, y))
                if x == m.column
                    && y == m.row
                    && now.duration_since(t).as_millis() < DOUBLE_CLICK_MS
        );
        if is_double {
            mute::clear_all();
            state.player.last_left_click = None;
            return;
        }
        state.player.last_left_click = Some((now, m.column, m.row));
    }

    // 1. Spiral.
    if let Some(spiral) = state.spiral.as_ref() {
        let rect = spiral.rect;
        if rect.width > 0
            && rect.height > 0
            && m.column >= rect.x
            && m.row >= rect.y
            && m.column < rect.x + rect.width
            && m.row < rect.y + rect.height
        {
            let lx = (m.column - rect.x) as usize;
            let ly = (m.row - rect.y) as usize;
            let gw = spiral.layout.grid_w as usize;
            let idx = ly * gw + lx;
            if let Some(Some(measure)) = spiral.layout.map.get(idx).copied() {
                match button {
                    MouseButton::Left => {
                        let sample = p.line_start_sample(measure);
                        tp.seek_to.store(sample, Ordering::Release);
                        state.player.last_loop_check_measure = None;
                    }
                    MouseButton::Right => {
                        if !state.player.loop_measures.remove(&measure) {
                            state.player.loop_measures.insert(measure);
                        }
                        // Adding a measure to the loop set turns on
                        // loop playback.  Users shouldn't have to know
                        // about the `L` key just to loop one measure.
                        if !state.player.loop_measures.is_empty() {
                            state.player.loop_enabled = true;
                        }
                        state.player.last_loop_check_measure = None;
                    }
                    _ => {}
                }
                return;
            }
        }
    }

    // 2. Tracker.
    if let Some(hit) = state.tracker_hit.as_ref() {
        let inner = hit.inner;
        if m.column >= inner.x
            && m.row >= inner.y
            && m.column < inner.x + inner.width
            && m.row < inner.y + inner.height
        {
            let rel_x = m.column - inner.x;
            let rel_y = m.row - inner.y;
            for col in &hit.columns {
                if rel_x < col.x_start || rel_x >= col.x_start + col.width {
                    continue;
                }
                let path = col.path.clone();
                if rel_y == col.header_y {
                    match button {
                        MouseButton::Left => {
                            mute::toggle_part_solo(&path);
                            state.player.focused_part = Some(path.clone());
                        }
                        MouseButton::Right => {
                            mute::toggle_part_mute(&path);
                        }
                        _ => {}
                    }
                } else if rel_y > col.header_y {
                    let row_idx = (rel_y - col.header_y - 1) as usize;
                    if let Some(Some((block_line, start_row))) =
                        col.rows.get(row_idx).copied()
                    {
                        match button {
                            MouseButton::Left => {
                                let block_measure_samples =
                                    p.line_measure_samples(block_line).max(1);
                                let rpmeasure = p.rows_per_measure().max(1.0);
                                let block_start = p.line_start_sample(block_line);
                                let offset = ((start_row as f64)
                                    / rpmeasure
                                    * (block_measure_samples as f64))
                                    as i64;
                                tp.seek_to
                                    .store(block_start + offset, Ordering::Release);
                                state.player.last_loop_check_measure = None;
                            }
                            MouseButton::Right => {
                                mute::toggle_note_mute(&path, block_line, start_row);
                            }
                            _ => {}
                        }
                    }
                }
                return;
            }
        }
    }
}