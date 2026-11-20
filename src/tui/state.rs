use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::crossterm::event::KeyCode;

use crate::tui::config::Settings;
use crate::tui::widgets::notch::config::ConfigUi;
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

/// Number of keyboard actions in the idle menu.  Keep in sync with
/// `tui::widgets::notch::menu::draw` and the match in `tui::app`.
pub const MENU_ITEMS: usize = 7;

/// One idle-menu action and the single argument that can be selected for it.
/// `values[i]` is passed to the underlying CLI when `args[i]` is displayed.
#[derive(Debug, Clone, Copy)]
pub struct MenuOption {
    pub label: &'static str,
    pub args: &'static [&'static str],
    pub values: &'static [&'static str],
}

pub const MENU_OPTIONS: [MenuOption; MENU_ITEMS] = [
    MenuOption {
        label: "PLAY",
        args: &["normal", "no cache", "no master fx", "no watch"],
        values: &["", "--no-cache", "--no-master-fx", "--no-watch"],
    },
    MenuOption {
        label: "DUMP",
        args: &["states", "no states", "raw"],
        values: &["", "--no-states", "--raw"],
    },
    MenuOption {
        label: "EXPORT",
        args: &["folder", "archive"],
        values: &["", "--archive"],
    },
    MenuOption {
        label: "INSTALL DIRS",
        args: &["dirs", "fish command"],
        values: &["", "--fish-command"],
    },
    MenuOption {
        label: "DOCTOR",
        args: &["summary", "dirs"],
        values: &["--log-only", "--dirs"],
    },
    MenuOption {
        label: "FETCH PLUGINS",
        args: &["default"],
        values: &[""],
    },
    MenuOption {
        label: "EDIT DIRS",
        args: &["normal"],
        values: &[""],
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuSubmenu {
    /// The nested directories editor.  The main idle menu shows logs in its
    /// place until this is opened.
    Directories,
}

#[derive(Debug, Clone, Default)]
pub struct MenuState {
    pub selected_index: usize,
    /// `None` means the main idle menu; `Some(_)` overlays a nested menu.
    pub submenu: Option<MenuSubmenu>,
    /// Which argument is active for each menu option.  Empty until first
    /// use so `MenuState::default()` stays cheap.
    pub arg_index: Vec<usize>,
    pub edit_mode: Option<usize>,
    /// Insertion point inside the current row, counted in characters.
    pub edit_cursor: usize,
    /// The row being added, so Esc can discard it.
    pub edit_is_new: bool,
    /// Parallel to `directories`: which config group each row belongs to.
    pub directory_kinds: Vec<&'static str>,
    pub directories: Vec<String>,
    /// Built-in roots that are always checked; kept separate from the
    /// editable rows so users can see valid drop locations without copying.
    pub system_roots: Vec<crate::deps::layout::SearchRoot>,
    /// First built-in root to show when the list is taller than the panel.
    pub system_scroll: usize,
    pub status: Option<String>,
    /// Background menu actions write here; the event loop lifts the value
    /// into `status` on the next frame so "done" never overwrites the TUI.
    pub status_slot: Arc<Mutex<Option<String>>>,
}

impl MenuState {
    pub fn new() -> Self {
        let mut state = Self::default();
        state.ensure_arg_index();
        state.reload_directories();
        state
    }

    fn ensure_arg_index(&mut self) {
        self.arg_index.resize(MENU_ITEMS, 0);
    }

    pub fn open_directories(&mut self) {
        self.finish_edit();
        self.submenu = Some(MenuSubmenu::Directories);
        self.system_scroll = 0;
    }

    pub fn close_submenu(&mut self) {
        self.finish_edit();
        self.submenu = None;
    }

    /// Cycle the single active argument for a menu option.  Options with one
    /// argument are deliberately inert; wrapping keeps Left/Right predictable.
    pub fn cycle_arg(&mut self, index: usize, delta: isize) {
        if index >= MENU_ITEMS { return; }
        self.ensure_arg_index();
        let count = MENU_OPTIONS[index].args.len();
        if count <= 1 { return; }
        let current = self.arg_index[index] as isize;
        self.arg_index[index] = (current + delta).rem_euclid(count as isize) as usize;
    }

    /// Visible argument for an option, or `None` when it has no choices.
    pub fn arg_label(&self, index: usize) -> Option<&'static str> {
        let option = MENU_OPTIONS.get(index)?;
        if option.args.len() <= 1 { return None; }
        let at = self.arg_index.get(index).copied().unwrap_or(0);
        option.args.get(at).copied()
    }

    /// CLI flags for the active argument.  Empty for the default choice.
    pub fn selected_flags(&self, index: usize) -> Vec<String> {
        let Some(option) = MENU_OPTIONS.get(index) else { return Vec::new() };
        let at = self.arg_index.get(index).copied().unwrap_or(0);
        option.values.get(at)
        .map(|v| if v.is_empty() { Vec::new() } else { vec![(*v).to_string()] })
        .unwrap_or_default()
    }

    /// Start editing a row with the cursor at the end of its text.
    pub fn begin_edit(&mut self, index: usize, is_new: bool) {
        if index >= self.directories.len() { return; }
        self.edit_mode = Some(index);
        self.edit_is_new = is_new;
        self.edit_cursor = self.directories[index].chars().count();
    }

    /// Move between editable rows.  Up/Down do this while editing so the
    /// other roots can be changed without leaving edit mode.
    pub fn move_edit_row(&mut self, delta: isize) {
        let Some(index) = self.edit_mode else { return };
        if self.directories.is_empty() { return; }

        // A blank row from `+ ... DIR` should not linger when the user
        // navigates away from it.
        if self.edit_is_new && self.directories[index].trim().is_empty()
        && self.directories.len() > 1 {
            self.directories.remove(index);
            self.directory_kinds.remove(index);
            let next = if delta > 0 {
                index.min(self.directories.len() - 1)
            } else {
                index.saturating_sub(1)
            };
            self.edit_mode = Some(next);
            self.edit_is_new = false;
            self.edit_cursor = self.directories[next].chars().count();
            return;
        }

        let last = self.directories.len() - 1;
        let next = (index as isize + delta).clamp(0, last as isize) as usize;
        self.edit_mode = Some(next);
        self.edit_is_new = false;
        self.edit_cursor = self.directories[next].chars().count();
    }

    /// Move the insertion point within the current row.
    pub fn move_edit_cursor(&mut self, delta: isize) {
        let Some(index) = self.edit_mode else { return };
        let len = self.directories[index].chars().count() as isize;
        self.edit_cursor = (self.edit_cursor as isize + delta).clamp(0, len) as usize;
    }

    pub fn edit_home(&mut self) { self.edit_cursor = 0; }

    pub fn edit_end(&mut self) {
        if let Some(index) = self.edit_mode {
            self.edit_cursor = self.directories[index].chars().count();
        }
    }

    pub fn insert_edit_char(&mut self, c: char) {
        let Some(index) = self.edit_mode else { return };
        let text = &mut self.directories[index];
        let byte = char_to_byte_idx(text, self.edit_cursor);
        text.insert(byte, c);
        self.edit_cursor += 1;
    }

    pub fn backspace_edit(&mut self) {
        let Some(index) = self.edit_mode else { return };
        if self.edit_cursor == 0 { return; }
        let text = &mut self.directories[index];
        let byte = char_to_byte_idx(text, self.edit_cursor - 1);
        text.remove(byte);
        self.edit_cursor -= 1;
    }

    pub fn delete_edit(&mut self) {
        let Some(index) = self.edit_mode else { return };
        let text = &mut self.directories[index];
        if self.edit_cursor >= text.chars().count() { return; }
        let byte = char_to_byte_idx(text, self.edit_cursor);
        text.remove(byte);
    }

    /// Leave edit mode after Enter/Esc commits or discards the pending row.
    pub fn finish_edit(&mut self) {
        self.edit_mode = None;
        self.edit_is_new = false;
        self.edit_cursor = 0;
    }

    /// Reload the single editable user root.  Everything else Circolate
    /// checks is discovered automatically and shown in `system_roots`.
    pub fn reload_directories(&mut self) {
        self.finish_edit();
        self.directory_kinds.clear();
        self.directories.clear();

        let current = crate::paths::user_root()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
        self.directory_kinds.push("user_root");
        self.directories.push(current);

        // `always_checked_roots()` already removes anything inside the user
        // root and legacy roots, so each location appears only once.
        self.system_roots = crate::deps::layout::always_checked_roots();
        self.system_scroll = 0;
    }

    /// Persist the user root to Circolate's localshare settings.
    pub fn save_directories(&self) -> anyhow::Result<()> {
        let cfg = crate::paths::DirectoriesConfig {
            user_root: self.directories.first()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
            ..crate::paths::DirectoriesConfig::default()
        };
        crate::paths::save_user_directories(&cfg)
    }
}

#[cfg(test)]
mod menu_edit_tests {
    use super::*;

    fn fixture() -> MenuState {
        let mut menu = MenuState::default();
        menu.directories = vec!["abc".into(), "xy".into()];
        menu.directory_kinds = vec!["user_root", "user_root"];
        menu
    }

    #[test]
    fn edit_cursor_inserts_and_deletes_at_position() {
        let mut menu = fixture();
        menu.begin_edit(0, false);
        menu.edit_home();
        menu.insert_edit_char('Z');
        assert_eq!(menu.directories[0], "Zabc");
        assert_eq!(menu.edit_cursor, 1);

        menu.move_edit_cursor(1);
        menu.insert_edit_char('Q');
        assert_eq!(menu.directories[0], "ZaQbc");

        menu.backspace_edit();
        assert_eq!(menu.directories[0], "Zabc");
        assert_eq!(menu.edit_cursor, 2);

        menu.delete_edit();
        assert_eq!(menu.directories[0], "Zac");
        assert_eq!(menu.edit_cursor, 2);
    }

    #[test]
    fn up_and_down_switch_editable_rows() {
        let mut menu = fixture();
        menu.begin_edit(0, false);
        assert_eq!(menu.edit_cursor, 3);

        menu.move_edit_row(1);
        assert_eq!(menu.edit_mode, Some(1));
        assert_eq!(menu.edit_cursor, 2);

        menu.move_edit_row(-1);
        assert_eq!(menu.edit_mode, Some(0));
        assert_eq!(menu.edit_cursor, 3);
    }

    #[test]
    fn argument_cycling_wraps_and_ignores_single_choice_options() {
        let mut menu = MenuState::default();
        assert_eq!(menu.arg_label(1), Some("states"));

        menu.cycle_arg(1, 1);
        assert_eq!(menu.arg_label(1), Some("no states"));
        assert_eq!(menu.selected_flags(1), vec!["--no-states"]);

        menu.cycle_arg(1, 1);
        assert_eq!(menu.arg_label(1), Some("raw"));

        menu.cycle_arg(1, 1);
        assert_eq!(menu.arg_label(1), Some("states"));
        assert!(menu.selected_flags(1).is_empty());

        menu.cycle_arg(5, 1);
        assert_eq!(menu.arg_label(5), None);
        assert!(menu.selected_flags(5).is_empty());
    }

    #[test]
    fn each_option_has_a_matching_argument_and_flag() {
        for option in MENU_OPTIONS {
            assert_eq!(option.args.len(), option.values.len());
            assert!(!option.args.is_empty());
        }
    }

    #[test]
    fn directories_submenu_opens_and_closes_cleanly() {
        let mut menu = MenuState::default();
        menu.directories.push(String::new());
        menu.directory_kinds.push("user_root");
        assert_eq!(menu.submenu, None);

        menu.open_directories();
        assert_eq!(menu.submenu, Some(MenuSubmenu::Directories));
        menu.begin_edit(0, false);
        assert!(menu.edit_mode.is_some());

        menu.close_submenu();
        assert_eq!(menu.submenu, None);
        assert_eq!(menu.edit_mode, None);
    }
}

#[derive(Debug, Clone, Default)]
pub struct PlayerState {
    pub show_help: bool,
    pub show_config: bool,
    pub show_analysis: bool,
    pub show_perf: bool,
    pub loop_enabled: bool,
    pub loop_measures: HashSet<usize>,
    pub last_loop_check_measure: Option<usize>,
    pub last_left_click: Option<(Instant, u16, u16)>,
    pub last_reload_count: i64,
    pub reload_flash_until: Option<Instant>,
    pub show_mixer: bool, // <--- NEW
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
    Perf,
    Mixer, // <--- NEW
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
    pub motion_highlight_line: Option<(usize, u64)>,
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
    pub dot_matrices: Vec<Vec<bool>>,
    pub sample_env_cache: HashMap<(String, usize), Vec<(f32, f32)>>,
    pub synth_env_cache: HashMap<(String, usize), Vec<(f32, f32)>>,
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
            dot_matrices: Vec::new(),
            sample_env_cache: HashMap::new(),
            synth_env_cache: HashMap::new(),
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
            motion_highlight_line: None,
        }
    }
}
