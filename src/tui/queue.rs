//! Queue widget: editable playlist anchored to the bottom-right of the
//! tracker panel.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use crate::tui::theme;

pub const MAX_LOOP: u8 = 9;

pub const GUTTER_W: u16 = 3;                    // "[n]"
pub const TEXT_W: u16 = 9;                      // song-name cells
pub const BUF_W: u16 = GUTTER_W + TEXT_W;       // one buffer row
pub const VISIBLE_LINES: usize = 5;

/// top edge + transport row + buffer rows (the last buffer row sits on
/// the tracker's bottom border row).  Width depends on the spiral.
pub const NOTCH_H: u16 = 2 + VISIBLE_LINES as u16;

// ── state (unchanged) ──────────────────────────────────────────────────

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
    /// True while the box has keyboard focus (typing goes here).
    pub focused: bool,
}

impl Default for QueueState {
    fn default() -> Self {
        QueueState {
            // Fixed slots: one per visible line.
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
        // Slots are fixed, so backspace at the start of a line does nothing.
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
        let count = self.lines[row].count;
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
        let mut n = self.lines[row].count as i16 + delta as i16;
        while n < 1 { n += MAX_LOOP as i16; }
        while n > MAX_LOOP as i16 { n -= MAX_LOOP as i16; }
        self.lines[row].count = n as u8;
    }

    pub fn set_loop(&mut self, n: u8) {
        if self.lines.is_empty() { return; }
        let row = self.cursor.row.min(self.lines.len() - 1);
        self.lines[row].count = n.clamp(1, MAX_LOOP);
    }

    pub fn current_name(&self) -> Option<&str> {
        let line = self.lines.get(self.cursor.row)?;
        if line.is_blank() { None } else { Some(line.text.trim()) }
    }

    pub fn advance(&mut self) -> Option<String> {
        let cur = self.playing?;
        if self.lines[cur].count > 1 {
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
        self.playing = Some(next);
        let name = self.lines[next].text.trim().to_string();
        self.now_playing = Some(name.clone());
        self.elapsed_secs = 0.0;
        Some(name)
    }

    pub fn play_current(&mut self) -> Option<String> {
        let name = self.current_name()?.to_string();
        self.playing = Some(self.cursor.row);
        self.now_playing = Some(name.clone());
        self.elapsed_secs = 0.0;
        Some(name)
    }
}

// ── mouse ──────────────────────────────────────────────────────────────

/// Which transport button a click landed on, if any.
pub enum Click {
    None,
    Rewind,
    Play,
}

impl QueueState {
    /// A mouse press at (x, y), already known to be inside the box.
    /// Clicking a line focuses the box and puts the cursor there; clicking
    /// a line's `[n]` cycles its loop count (right button counts down).
    pub fn click(&mut self, p: &Placed, x: u16, y: u16, right_button: bool) -> Click {
        let hit = |r: Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        if hit(p.rewind) {
            return Click::Rewind;
        }
        if hit(p.play) {
            return Click::Play;
        }
        if !hit(p.lines) {
            return Click::None;
        }

        let row = p.first_line + (y - p.lines.y) as usize;
        if row >= self.lines.len() {
            return Click::None;
        }
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

// ── theme ──────────────────────────────────────────────────────────────

pub struct Theme {
    /// Solid background behind the buffer, so text stays legible over the
    /// cover mosaic.
    pub bg: Color,
    pub transport_active: Style,
    pub transport_idle: Style,
    pub queue_text: Style,
    pub queue_playing: Style,
    pub counter: Style,
    pub counter_dim: Style,
    pub cursor: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            bg: Color::Rgb(0, 0, 0),
            transport_active: Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            transport_idle: Style::default().fg(Color::DarkGray),
            queue_text: Style::default().fg(Color::Gray),
            queue_playing: Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            counter: Style::default().fg(Color::Yellow),
            counter_dim: Style::default().fg(Color::Rgb(70, 70, 70)),
            cursor: Style::default().bg(Color::White).fg(Color::Black),
        }
    }
}

// ── rendering ──────────────────────────────────────────────────────────

/// Where `render` put things, for the caller to draw the spiral into and
/// to hit-test mouse clicks against.
#[derive(Clone, Copy)]
pub struct Placed {
    /// The whole box, frame edges included.
    pub notch: Rect,
    /// Interior slot for the spiral.
    pub spiral: Rect,
    /// The ◀ button.
    pub rewind: Rect,
    /// The ▶ button and its speed label.
    pub play: Rect,
    /// Gutter + text area of all the visible lines.
    pub lines: Rect,
    /// Index of the line drawn on the first row of `lines`.
    pub first_line: usize,
}

/// Carve a notch out of the tracker frame's bottom-right corner and draw
/// the queue in it.  `tracker` is the tracker panel's full rect (border
/// included), already drawn.  `spiral_size` is the spiral layout's
/// (grid_w, grid_h); the box is as wide as the spiral needs.  Returns
/// None if the panel is too small.
pub fn render(
    state: &QueueState,
    tracker: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    spiral_size: (u16, u16),
) -> Option<Placed> {
    let sw = spiral_size.0.max(1);
    let sh = spiral_size.1.max(1).min(NOTCH_H - 1);
    let nw = 1 + BUF_W + sw;
    if tracker.width < nw + 2 || tracker.height < NOTCH_H + 3 {
        return None;
    }
    let set = theme::border_set(theme::BORDER_STYLE);
    // Reuse whatever colour the tracker's own border was drawn in.
    let edge = Style::default()
        .fg(buf.cell((tracker.x, tracker.y)).map_or(Color::Reset, |c| c.fg));

    let nx = tracker.right() - nw;
    let ny = tracker.bottom() - NOTCH_H;
    let right = tracker.right() - 1;
    let bottom = tracker.bottom() - 1;
    let bx = nx + 1; // first buffer column
    let sx = bx + BUF_W; // where the spiral starts

    // The buffer is a solid black "terminal".  The spiral side is only
    // cleared of tracker text; its background is left alone so the cover
    // shows through.
    blank(buf, Rect::new(bx, ny + 1, BUF_W, NOTCH_H - 1), theme.bg);
    clear_keep_bg(buf, Rect::new(sx, ny + 1, sw, NOTCH_H - 1));

    // Tracker frame, rerouted around the box.
    put(buf, right, ny, set.bottom_right, edge);
    for x in nx + 1..right {
        put(buf, x, ny, set.horizontal_top, edge);
    }
    put(buf, nx, ny, set.top_left, edge);
    for y in ny + 1..bottom {
        put(buf, nx, y, set.vertical_left, edge);
    }
    put(buf, nx, bottom, set.bottom_right, edge);

    // Transport buttons, right-aligned against the spiral.
    let label = state.speed_label.as_str();
    let lw = label.chars().count() as u16;
    let tx = sx - 1 - (3 + lw);
    let ty = ny + 1;
    let on = theme.transport_active;
    let off = theme.transport_idle;
    put(buf, tx, ty, "◀", if state.direction == '◀' { on } else { off });
    put(buf, tx + 2, ty, "▶", if state.direction == '▶' { on } else { off });
    put(buf, tx + 3, ty, label, if state.direction == '■' { off } else { on });

    // Lines: `[n]` gutter, then the song name.
    let first = state.cursor.row.saturating_sub(VISIBLE_LINES - 1);
    let lines_y = ny + 2;
    let text_x = bx + GUTTER_W;
    for i in 0..VISIBLE_LINES {
        let idx = first + i;
        let y = lines_y + i as u16;
        let Some(line) = state.lines.get(idx) else { continue };

        let st = if line.count > 1 { theme.counter } else { theme.counter_dim };
        put(buf, bx, y, &format!("[{}]", line.count), st);

        let text_style = if state.playing == Some(idx) { theme.queue_playing } else { theme.queue_text };
        let show_cursor = state.focused && state.cursor.row == idx;
        for (ci, ch) in line.text.chars().enumerate().take(TEXT_W as usize) {
            let st = if show_cursor && ci == state.cursor.col { theme.cursor } else { text_style };
            put(buf, text_x + ci as u16, y, ch.encode_utf8(&mut [0u8; 4]), st);
        }
        let n = line.text.chars().count();
        if show_cursor && state.cursor.col >= n && state.cursor.col < TEXT_W as usize {
            put(buf, text_x + state.cursor.col as u16, y, " ", theme.cursor);
        }
    }

    Some(Placed {
        notch: Rect::new(nx, ny, nw, NOTCH_H),
        // Bottom-aligned, so the spiral's last ring sits on the frame's bottom row.
        spiral: Rect::new(sx, bottom + 1 - sh, sw, sh),
        rewind: Rect::new(tx, ty, 2, 1),
        play: Rect::new(tx + 2, ty, 1 + lw, 1),
        lines: Rect::new(bx, lines_y, BUF_W, VISIBLE_LINES as u16),
        first_line: first,
    })
}

// ── helpers ────────────────────────────────────────────────────────────

fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) {
    let a = *buf.area();
    if x < a.x || y < a.y || x >= a.right() || y >= a.bottom() {
        return;
    }
    buf.set_stringn(x, y, s, (a.right() - x) as usize, style);
}

fn blank(buf: &mut Buffer, r: Rect, bg: Color) {
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.reset();
                c.set_bg(bg);
            }
        }
    }
}

/// Erase symbols and colours but keep the cell's background, so the cover
/// mosaic stays visible.
fn clear_keep_bg(buf: &mut Buffer, r: Rect) {
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                let bg = c.bg;
                c.reset();
                c.set_bg(bg);
            }
        }
    }
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}