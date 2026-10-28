//! TUI-side state.  Nothing in here belongs to `Project`, the
//! transport, or the mixer — this is what the event loop threads
//! through each frame.  Views read it; the event loop mutates it.

use crossterm::event::KeyCode;
use ratatui::layout::Rect;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use crate::tui::config::Settings;
use crate::tui::spiral::SpiralLayout;
use crate::tui::widgets::cover::Cover;

// ── queue ──────────────────────────────────────────────────────────────

pub const MAX_LOOP: u8 = 9;
pub const GUTTER_W: u16 = 3;
pub const TEXT_W: u16 = 9;
pub const BUF_W: u16 = GUTTER_W + TEXT_W;
pub const VISIBLE_LINES: usize = 5;

#[derive(Debug, Clone)]
pub struct QueueLine {
    pub text: String,
    pub count: u8,
}

impl QueueLine {
    pub fn new(text: impl Into<String>) -> Self {
        QueueLine { text: text.into(), count: 1 }
    }
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone)]
pub struct QueueState {
    pub lines: Vec<QueueLine>,
    pub cursor: Cursor,
    pub playing: Option<usize>,
    pub now_playing: Option<String>,
    pub elapsed_secs: f64,
    pub total_secs: f64,
    pub speed_label: String,
    pub direction: char,
    pub has_cover: bool,
    pub focused: bool,
}

impl Default for QueueState {
    fn default() -> Self {
        QueueState {
            lines: (0..VISIBLE_LINES).map(|_| QueueLine::new("")).collect(),
            cursor: Cursor::default(),
            playing: None,
            now_playing: None,
            elapsed_secs: 0.0,
            total_secs: 0.0,
            speed_label: "1x".into(),
            direction: '■',
            has_cover: false,
            focused: false,
        }
    }
}

impl QueueState {
    pub fn new() -> Self { Self::default() }

    pub fn insert_char(&mut self, c: char) {
        if self.lines.is_empty() { self.lines.push(QueueLine::new("")); }
        let row = self.cursor.row.min(self.lines.len() - 1);
        if self.lines[row].text.chars().count() >= TEXT_W as usize { return; }
        let byte = char_to_byte(&self.lines[row].text, self.cursor.col);
        self.lines[row].text.insert(byte, c);
        self.cursor.col += 1;
    }

    pub fn backspace(&mut self) {
        if self.lines.is_empty() { return; }
        let row = self.cursor.row.min(self.lines.len() - 1);
        if self.cursor.col == 0 { return; }
        let line = &mut self.lines[row];
        let end = char_to_byte(&line.text, self.cursor.col);
        let start = char_to_byte(&line.text, self.cursor.col - 1);
        line.text.replace_range(start..end, "");
        self.cursor.col -= 1;
    }

    pub fn delete_char(&mut self) {
        if self.lines.is_empty() { return; }
        let row = self.cursor.row.min(self.lines.len() - 1);
        let line = &mut self.lines[row];
        let n = line.text.chars().count();
        if self.cursor.col >= n { return; }
        let start = char_to_byte(&line.text, self.cursor.col);
        let end = char_to_byte(&line.text, self.cursor.col + 1);
        line.text.replace_range(start..end, "");
    }

    pub fn new_line(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(QueueLine::new(""));
            return;
        }
        let row = self.cursor.row.min(self.lines.len() - 1);
        let col = self.cursor.col;
        let split = char_to_byte(&self.lines[row].text, col);
        let tail = self.lines[row].text.split_off(split);
        let count = self.lines[row].count.max(1);
        self.lines.insert(row + 1, QueueLine { text: tail, count });
        self.cursor.row = row + 1;
        self.cursor.col = 0;
    }

    pub fn cursor_up(&mut self) {
        if self.cursor.row > 0 {
            self.cursor.row -= 1;
            let n = self.lines[self.cursor.row].text.chars().count();
            self.cursor.col = self.cursor.col.min(n);
        }
    }
    pub fn cursor_down(&mut self) {
        if self.cursor.row + 1 < self.lines.len() {
            self.cursor.row += 1;
            let n = self.lines[self.cursor.row].text.chars().count();
            self.cursor.col = self.cursor.col.min(n);
        }
    }
    pub fn cursor_left(&mut self) {
        if self.cursor.col > 0 { self.cursor.col -= 1; }
        else if self.cursor.row > 0 {
            self.cursor.row -= 1;
            self.cursor.col = self.lines[self.cursor.row].text.chars().count();
        }
    }
    pub fn cursor_right(&mut self) {
        let n = self.lines[self.cursor.row].text.chars().count();
        if self.cursor.col < n { self.cursor.col += 1; }
        else if self.cursor.row + 1 < self.lines.len() {
            self.cursor.row += 1;
            self.cursor.col = 0;
        }
    }
    pub fn cursor_home(&mut self) { self.cursor.col = 0; }
    pub fn cursor_end(&mut self) {
        self.cursor.col = self.lines[self.cursor.row].text.chars().count();
    }

    pub fn cycle_loop(&mut self, delta: i8) {
        if self.lines.is_empty() { return; }
        let row = self.cursor.row.min(self.lines.len() - 1);
        let lo: i16 = if self.playing == Some(row) { 0 } else { 1 };
        let span = MAX_LOOP as i16 - lo + 1;
        let mut n = self.lines[row].count as i16 + delta as i16;
        while n < lo { n += span; }
        while n > MAX_LOOP as i16 { n -= span; }
        self.lines[row].count = n as u8;
    }

    pub fn set_loop(&mut self, n: u8) {
        if self.lines.is_empty() { return; }
        let row = self.cursor.row.min(self.lines.len() - 1);
        let lo = if self.playing == Some(row) { 0 } else { 1 };
        self.lines[row].count = n.clamp(lo, MAX_LOOP);
    }

    fn start_line(&mut self, row: usize) -> String {
        let name = self.lines[row].text.trim().to_string();
        self.lines[row].count = self.lines[row].count.max(1) - 1;
        self.playing = Some(row);
        self.now_playing = Some(name.clone());
        self.elapsed_secs = 0.0;
        name
    }

    pub fn start_at(&mut self, row: usize) -> Option<String> {
        if row >= self.lines.len() || self.lines[row].is_blank() { return None; }
        Some(self.start_line(row))
    }

    pub fn current_name(&self) -> Option<&str> {
        let line = self.lines.get(self.cursor.row)?;
        if line.is_blank() { None } else { Some(line.text.trim()) }
    }

    pub fn advance(&mut self) -> Option<String> {
        let cur = self.playing?;
        if self.lines[cur].count > 0 {
            self.lines[cur].count -= 1;
            self.elapsed_secs = 0.0;
            return Some(self.lines[cur].text.trim().to_string());
        }
        let mut next = cur + 1;
        while next < self.lines.len() && self.lines[next].is_blank() { next += 1; }
        if next >= self.lines.len() {
            self.playing = None;
            self.now_playing = None;
            self.elapsed_secs = 0.0;
            self.total_secs = 0.0;
            return None;
        }
        Some(self.start_line(next))
    }

    pub fn play_current(&mut self) -> Option<String> {
        let name = self.current_name()?.to_string();
        let row = self.cursor.row;
        self.start_line(row);
        Some(name)
    }

    /// A mouse press at (x, y), already known to be inside the box.
    pub fn click(&mut self, p: &Placed, x: u16, y: u16, right_button: bool) -> Click {
        let hit = |r: Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        if hit(p.rewind) { return Click::Rewind; }
        if hit(p.play)   { return Click::Play; }
        if !hit(p.lines) { return Click::None; }

        let row = p.first_line + (y - p.lines.y) as usize;
        if row >= self.lines.len() { return Click::None; }
        self.cursor.row = row;
        self.focused = true;
        let col = (x - p.lines.x) as usize;
        let len = self.lines[row].text.chars().count();
        if col < GUTTER_W as usize {
            self.cycle_loop(if right_button { -1 } else { 1 });
            self.cursor.col = self.cursor.col.min(len);
        } else {
            self.cursor.col = (col - GUTTER_W as usize).min(len);
        }
        Click::None
    }
}

/// Which transport button a click landed on, if any.
pub enum Click { None, Rewind, Play }

/// Where the queue widget drew things, for the event loop to hit-test.
#[derive(Clone, Copy)]
pub struct Placed {
    pub box_rect: Rect,
    pub spiral: Rect,
    pub rewind: Rect,
    pub play: Rect,
    pub lines: Rect,
    pub first_line: usize,
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

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

// ── spiral hit-test record ─────────────────────────────────────────────

/// Where the spiral widget drew itself.  `widgets/spiral.rs` fills this
/// in each frame; `shared/hit.rs` reads it to route a click to a
/// measure.
pub struct SpiralState {
    pub layout: SpiralLayout,
    pub rect: Rect,
}

// ── status notch hit-test record ───────────────────────────────────────

/// What a clickable segment of the notch does.  Extend as the notch
/// grows; `shared/hit.rs` maps each variant to a `HitAction`.
#[derive(Clone, Copy)]
pub enum NotchTag {
    /// Toggle the help overlay.
    Help,
    /// Toggle the config overlay.
    Settings,
}

/// Where the notch drew its clickable segments, for `shared/hit.rs`.
#[derive(Clone)]
pub struct NotchPlaced {
    pub rects: Vec<(NotchTag, Rect)>,
}

// ── click targets ──────────────────────────────────────────────────────

pub struct TrackerColumnHit {
    pub path: String,
    pub x_start: u16,
    pub width: u16,
    pub header_y: u16,
    pub rows: Vec<Option<(usize, i64)>>,
    /// Inner-relative `x` range of the `@` / `↴` chain marker on the
    /// header row.  `None` when the column has no chain.
    pub marker_x: Option<(u16, u16)>,
}

pub struct TrackerHit {
    pub inner: Rect,
    pub columns: Vec<TrackerColumnHit>,
}

/// One interactive row in the vertical tracker.  Populated each frame
/// by `views::vertical`; a click at (column, row) looks up here.
#[derive(Clone)]
pub enum VerticalRow {
    /// The part's name line.  Left-click solos, right-click mutes.
    /// `marker_x` is the `@` / `↦` chain marker range on that row.
    Label { path: String, marker_x: Option<(u16, u16)> },
    /// A line containing one or more events.  `block_line` and
    /// `start_row` identify the first note on that line.
    Event { path: String, block_line: usize, start_row: i64 },
    /// A waveform row.  Click-to-seek uses the click's x-position as a
    /// fraction of the view's inner width.
    Waveform { path: String, line: usize },
}

pub struct VerticalHit {
    pub inner: Rect,
    pub rows: Vec<(u16, VerticalRow)>,
}

// ── player UI state ────────────────────────────────────────────────────

pub struct PlayerUi {
    pub loop_measures: HashSet<usize>,
    pub loop_enabled: bool,
    pub last_loop_check_measure: Option<usize>,
    pub show_help: bool,
    pub show_config: bool,
    pub last_left_click: Option<(Instant, u16, u16)>,
    pub last_reload_count: i64,
    pub reload_flash_until: Option<Instant>,
}

impl Default for PlayerUi {
    fn default() -> Self {
        Self {
            loop_measures: HashSet::new(),
            loop_enabled: false,
            last_loop_check_measure: None,
            show_help: false,
            show_config: false,
            last_left_click: None,
            last_reload_count: 0,
            reload_flash_until: None,
        }
    }
}

// ── render state ───────────────────────────────────────────────────────

pub struct RenderState {
    pub settings: Settings,
    pub last_synth_env: Vec<Vec<(f32, f32)>>,
    pub snapshot: Vec<f32>,
    pub column_order: Vec<String>,
    pub cover: Option<Cover>,
    pub cover_source: Option<PathBuf>,
    pub cover_dims: (usize, usize),
    pub last_dir_press: Option<(KeyCode, Instant)>,
    pub spiral: Option<SpiralState>,
    pub notch: Option<NotchPlaced>,
    pub playheads: HashMap<String, ColumnPlayhead>,
    pub sample_env_cache: HashMap<(String, usize), Vec<(f32, f32)>>,
    pub player: PlayerUi,
    pub tracker_hit: Option<TrackerHit>,
    pub vertical_hit: Option<VerticalHit>,
    pub volume_state: Option<(f64, bool)>,
    pub volume_last_poll: Instant,
    pub queue: QueueState,
    pub queue_placed: Option<Placed>,
    pub play_request: Option<String>,
    pub loading: Option<String>,
    pub loading_notes: Vec<String>,
    pub rewind_cooldown_until: Option<Instant>,
    pub config_ui: crate::tui::widgets::config::ConfigUi,
    /// Part paths whose chain is currently expanded (`↴` / `↦` shown).
    pub expanded_chains: HashSet<String>,
    /// Part paths whose chain has been toggled disabled.  Currently
    /// visual-only — audio bypass is a follow-up.
    pub disabled_chains: HashSet<String>,
}

impl RenderState {
    pub fn new(queue: QueueState, settings: Settings) -> Self {
        Self {
            settings,
            last_synth_env: Vec::new(),
            snapshot: Vec::with_capacity(crate::player::scope::SCOPE_LEN),
            column_order: Vec::new(),
            cover: None,
            cover_source: None,
            cover_dims: (0, 0),
            last_dir_press: None,
            spiral: None,
            notch: None,
            playheads: HashMap::new(),
            sample_env_cache: HashMap::new(),
            player: PlayerUi::default(),
            tracker_hit: None,
            vertical_hit: None,
            volume_state: None,
            volume_last_poll: Instant::now(),
            queue,
            queue_placed: None,
            play_request: None,
            loading: None,
            loading_notes: Vec::new(),
            rewind_cooldown_until: None,
            config_ui: Default::default(),
            expanded_chains: HashSet::new(),
            disabled_chains: HashSet::new(),
        }
    }
}
