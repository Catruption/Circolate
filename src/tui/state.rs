use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::crossterm::event::KeyCode;

use crate::tui::config::Settings;
use crate::tui::widgets::config::ConfigUi;
use crate::tui::widgets::cover::Cover;
use crate::tui::spiral::SpiralLayout;
use crate::tui::layout::QueueLayout;

pub const VISIBLE_LINES: usize = 5;
pub const GUTTER_W: u16 = 3;
pub const TEXT_W: u16 = 50;
pub const BUF_W: u16 = 20;

#[derive(Clone, PartialEq, Eq)]
pub struct SpiralCacheKey {
    pub total_measures: usize,
    pub aspect: usize,
    pub budget_w: usize,
    pub budget_h: usize,
    pub tape_left: u16,
    pub area: Rect,
    pub tape_rows: u16,
    pub speed_label_w: u16,
}

pub struct SpiralCache {
    pub key: SpiralCacheKey,
    pub result: Option<(SpiralLayout, QueueLayout)>,
}

#[derive(Debug, Clone, Default)]
pub struct MenuState {
    pub selected_index: usize,
    pub edit_mode: Option<usize>,
    pub directories: Vec<String>,
}

impl MenuState {
    pub fn new() -> Self {
        let mut directories = Vec::new();
        directories.push(crate::paths::user_trackscripts().to_string_lossy().into_owned());
        directories.push(crate::paths::bundled_trackscripts().to_string_lossy().into_owned());
        Self {
            selected_index: 0,
            edit_mode: None,
            directories,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PlayerState {
    pub show_help: bool,
    pub show_config: bool,
    pub show_analysis: bool,
    pub loop_enabled: bool,
    pub loop_measures: HashSet<usize>,
    pub last_loop_check_measure: Option<usize>,
    pub last_left_click: Option<(Instant, u16, u16)>,
    pub last_reload_count: i64,
    pub reload_flash_until: Option<Instant>,
}

#[derive(Debug, Clone, Default)]
pub struct QueueLine {
    pub text: String,
    pub count: usize,
    pub is_blank: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone)]
pub struct QueueState {
    pub lines: Vec<QueueLine>,
    pub cursor: Cursor,
    pub focused: bool,
    pub playing: Option<usize>,
    pub now_playing: Option<String>,
    pub direction: char,
    pub speed_label: String,
}

fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
    .nth(char_idx)
    .map(|(i, _)| i)
    .unwrap_or(s.len())
}

impl QueueState {
    pub fn new() -> Self {
        Self {
            lines: vec![QueueLine { is_blank: true, ..Default::default() }; VISIBLE_LINES],
            cursor: Cursor::default(),
            focused: false,
            playing: None,
            now_playing: None,
            direction: '■',
            speed_label: String::new(),
        }
    }

    pub fn insert_char(&mut self, c: char) {
        let row = self.cursor.row;
        if row >= self.lines.len() { return; }
        let text_len = self.lines[row].text.chars().count();
        if text_len >= TEXT_W as usize { return; }
        let col = self.cursor.col.min(text_len);
        let byte_idx = char_to_byte_idx(&self.lines[row].text, col);
        self.lines[row].text.insert(byte_idx, c);
        self.lines[row].is_blank = false;
        self.cursor.col = col + 1;
    }

    pub fn backspace(&mut self) {
        let row = self.cursor.row;
        if row >= self.lines.len() || self.cursor.col == 0 { return; }
        let col = self.cursor.col.min(self.lines[row].text.chars().count());
        if col == 0 { return; }
        let byte_idx = char_to_byte_idx(&self.lines[row].text, col - 1);
        self.lines[row].text.remove(byte_idx);
        self.cursor.col = col - 1;
        self.lines[row].is_blank = self.lines[row].text.is_empty();
    }

    pub fn delete_char(&mut self) {
        let row = self.cursor.row;
        if row >= self.lines.len() { return; }
        let text_len = self.lines[row].text.chars().count();
        if self.cursor.col >= text_len { return; }
        let byte_idx = char_to_byte_idx(&self.lines[row].text, self.cursor.col);
        self.lines[row].text.remove(byte_idx);
        self.lines[row].is_blank = self.lines[row].text.is_empty();
    }

    pub fn cursor_left(&mut self) {
        self.cursor.col = self.cursor.col.saturating_sub(1);
    }

    pub fn cursor_right(&mut self) {
        let row = self.cursor.row;
        let text_len = self.lines.get(row)
        .map(|l| l.text.chars().count())
        .unwrap_or(0);
        if self.cursor.col < text_len {
            self.cursor.col += 1;
        }
    }

    pub fn cursor_up(&mut self) {
        if self.cursor.row > 0 {
            self.cursor.row -= 1;
            let text_len = self.lines[self.cursor.row].text.chars().count();
            self.cursor.col = self.cursor.col.min(text_len);
        }
    }

    pub fn cursor_down(&mut self) {
        if self.cursor.row + 1 < self.lines.len() {
            self.cursor.row += 1;
            let text_len = self.lines[self.cursor.row].text.chars().count();
            self.cursor.col = self.cursor.col.min(text_len);
        }
    }

    pub fn cursor_home(&mut self) {
        self.cursor.col = 0;
    }

    pub fn cursor_end(&mut self) {
        let row = self.cursor.row;
        self.cursor.col = self.lines.get(row)
        .map(|l| l.text.chars().count())
        .unwrap_or(0);
    }

    pub fn cycle_loop(&mut self, dir: i32) {
        const MAX_LOOP: i32 = 9;

        let row = self.cursor.row;
        if row >= self.lines.len() { return; }

        let line = &mut self.lines[row];

        let lo: i32 = if self.playing == Some(row) { 0 } else { 1 };
        let span = MAX_LOOP - lo + 1;

        let mut n = line.count as i32 + dir;

        while n < lo { n += span; }
        while n > MAX_LOOP { n -= span; }

        line.count = n as usize;
    }

    pub fn advance(&mut self) -> Option<String> {
        let cur = self.playing?;

        if let Some(line) = self.lines.get_mut(cur) {
            if line.count > 0 {
                line.count -= 1;
                let text = line.text.clone();
                self.now_playing = Some(text.clone());
                return Some(text);
            }
        }

        let next = (cur + 1..self.lines.len())
        .find(|&i| !self.lines[i].text.is_empty());
        match next {
            Some(i) => {
                self.playing = Some(i);
                let text = self.lines[i].text.clone();
                self.now_playing = Some(text.clone());
                Some(text)
            }
            None => {
                self.playing = None;
                self.now_playing = None;
                None
            }
        }
    }

    pub fn play_current(&mut self) -> Option<String> {
        let row = self.cursor.row;
        let line = self.lines.get(row)?;
        if line.text.is_empty() {
            return None;
        }
        self.playing = Some(row);
        self.now_playing = Some(line.text.clone());
        Some(line.text.clone())
    }

    pub fn start_at(&mut self, row: usize) {
        if row >= self.lines.len() { return; }
        self.playing = Some(row);
        self.now_playing = Some(self.lines[row].text.clone());
    }

    #[allow(dead_code)]
    pub fn click(&mut self, _q: &Placed, _col: u16, _row: u16, _right: bool) -> Click {
        Click::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotchTag {
    Help,
    Settings,
    Analysis,
}

#[derive(Debug, Clone)]
pub struct NotchPlaced {
    pub rects: Vec<(NotchTag, Rect)>,
}

#[derive(Debug, Clone, Copy)]
pub struct Placed {
    pub box_rect: Rect,
    pub spiral: Rect,
    pub rewind: Rect,
    pub play: Rect,
    pub lines: Rect,
    pub first_line: usize,
}

#[derive(Debug, Clone)]
pub struct SpiralState {
    pub layout: SpiralLayout,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    None,
    Rewind,
    Play,
}

#[derive(Debug, Clone)]
pub enum VerticalRow {
    Label { path: String, marker_x: Option<(u16, u16)> },
    Event { path: String, block_line: usize, start_row: i64 },
    Waveform { path: String, line: usize },
}

#[derive(Debug, Clone)]
pub struct VerticalHit {
    pub inner: Rect,
    pub rows: Vec<(u16, VerticalRow)>,
}

#[derive(Debug, Clone)]
pub struct TrackerColumnHit {
    pub path: String,
    pub x_start: u16,
    pub width: u16,
    pub header_y: u16,
    pub rows: Vec<Option<(usize, i64)>>,
    pub marker_x: Option<(u16, u16)>,
}

#[derive(Debug, Clone)]
pub struct TrackerHit {
    pub inner: Rect,
    pub columns: Vec<TrackerColumnHit>,
}

#[derive(Debug, Clone, Default)]
pub struct PlayheadState {
    pub position: f64,
    pub note_id: Option<(String, f64, i64)>,
    pub last_audio_pos: i64,
}

pub struct RenderState {
    pub motion_highlights: Vec<(usize, i64)>,
    pub settings: Settings,
    pub config_ui: ConfigUi,
    pub player: PlayerState,
    pub queue: QueueState,
    pub queue_placed: Option<Placed>,
    pub spiral: Option<SpiralState>,
    pub notch: Option<NotchPlaced>,
    pub cover: Option<Cover>,
    pub cover_source: Option<PathBuf>,
    pub cover_dims: (usize, usize),

    pub column_order: Vec<String>,
    pub vertical_hit: Option<VerticalHit>,
    pub tracker_hit: Option<TrackerHit>,

    pub playheads: HashMap<String, PlayheadState>,
    pub last_synth_env: Vec<Vec<(f32, f32)>>,
    pub sample_env_cache: HashMap<(String, usize), Vec<(f32, f32)>>,
    pub snapshot: Vec<f32>,

    pub expanded_chains: HashSet<String>,
    pub disabled_chains: HashSet<String>,

    pub loading: Option<String>,
    pub loading_notes: Vec<String>,
    pub play_request: Option<String>,

    pub volume_last_poll: Instant,
    pub last_dir_press: Option<(KeyCode, Instant)>,
    pub rewind_cooldown_until: Option<Instant>,

    pub perf_last_poll: Instant,
    pub(crate) perf_snapshot: Option<crate::player::perf::PerfSnapshot>,
    pub spiral_cache: Option<SpiralCache>,

    pub menu: MenuState,
}

impl RenderState {
    pub fn new(queue: QueueState, settings: Settings) -> Self {
        Self {
            settings,
            config_ui: ConfigUi::default(),
            player: PlayerState::default(),
            queue,
            queue_placed: None,
            spiral: None,
            notch: None,
            cover: None,
            cover_source: None,
            cover_dims: (0, 0),

            column_order: Vec::new(),
            vertical_hit: None,
            tracker_hit: None,

            playheads: HashMap::new(),
            last_synth_env: Vec::new(),
            sample_env_cache: HashMap::new(),
            snapshot: Vec::new(),

            expanded_chains: HashSet::new(),
            disabled_chains: HashSet::new(),

            loading: None,
            loading_notes: Vec::new(),
            play_request: None,

            volume_last_poll: Instant::now(),
            last_dir_press: None,
            rewind_cooldown_until: None,

            perf_last_poll: Instant::now(),
            perf_snapshot: None,
            spiral_cache: None,

            menu: MenuState::new(),
            motion_highlights: Vec::new(),
        }
    }
}
