//! Modal settings overlay. Same chrome as `help.rs`; adds row
//! navigation, `←→` adjustment, undo, and a color picker.
//!
//! Three modes: `List` (all settings), `Colors` (32 theme tokens with
//! live swatches), `Picker` (5×8 palette grid).  Edits mutate
//! `Settings` directly so the overlay's own border and colors update
//! on the next frame — `←→` on `border` redraws the box around you.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::paths;
use crate::tui::config::{Config, Settings};
use crate::tui::theme::{builtin, BorderStyle, Token};

// ── state ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    List,
    Colors,
    Picker,
}

pub struct ConfigUi {
    pub mode: Mode,
    pub cursor: usize,
    pub color_cursor: usize,
    pub picker_cursor: (usize, usize),
    pub undo: Vec<Settings>,
    pub dirty: bool,
}

impl Default for ConfigUi {
    fn default() -> Self {
        ConfigUi {
            mode: Mode::List,
            cursor: 0,
            color_cursor: 0,
            picker_cursor: (0, 0),
            undo: Vec::new(),
            dirty: false,
        }
    }
}

impl ConfigUi {
    /// Called when the overlay opens. Snapshots `Settings` as the base
    /// undo entry; positions the cursor on the first editable row.
    pub fn open(&mut self, settings: &Settings) {
        self.mode = Mode::List;
        self.cursor = 0;
        while matches!(ROWS.get(self.cursor), Some(Row::Header(_))) {
            self.cursor += 1;
        }
        self.undo.clear();
        self.undo.push(settings.clone());
        self.dirty = false;
    }
}

pub enum Action {
    Continue,
    Close,
}

// ── row schema ────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Row {
    Header(&'static str),
    Enum(&'static str, &'static [&'static str]),
    Border,
    Bool(&'static str),
    Int(&'static str, i64, i64),
    Float(&'static str, f64, f64, f64),
    Glyph(&'static str, &'static [&'static str]),
    Overrides,
}

const ROWS: &[Row] = &[
    Row::Header("theme"),
    Row::Border,
Row::Header("view"),
Row::Enum("view mode", &["auto", "horizontal", "vertical"]),
Row::Header("layout"),
Row::Float("cover_dim", 0.0, 2.0, 0.05),
Row::Int("spiral_aspect_x", 1, 8),
Row::Int("spiral_max_h", 2, 30),
Row::Int("min_col_w", 4, 60),
Row::Int("max_col_w", 4, 80),
Row::Int("lookback_lines", 0, 512),
Row::Glyph(
    "playhead_marker",
    &["━", "─", "│", "▌", "▐", "█", "═", "-"],
),
Row::Glyph("help_glyph", &["?", "h", "ⓘ", "i", "◆", "▪"]),
Row::Header("labels"),
Row::Bool("accent_number"),
Row::Bool("accent_fn"),
Row::Bool("accent_instance"),
Row::Header("synesthesia"),
Row::Bool("enabled"),
Row::Float("hue_step", 0.0, 360.0, 5.0),
Row::Float("saturation_scale", 0.0, 2.0, 0.05),
Row::Float("value", 0.0, 2.0, 0.05),
Row::Header("colors"),
Row::Overrides,
];

fn is_editable(r: Option<&Row>) -> bool {
    matches!(
        r,
        Some(Row::Enum(..))
        | Some(Row::Border)
        | Some(Row::Bool(_))
        | Some(Row::Int(..))
        | Some(Row::Float(..))
        | Some(Row::Glyph(..))
        | Some(Row::Overrides)
    )
}

// ── palette ───────────────────────────────────────────────────────────

const PALETTE: [[Color; 8]; 5] = [
    // Saturated
    [
        Color::Rgb(255, 80, 80),
        Color::Rgb(255, 150, 50),
        Color::Rgb(255, 230, 60),
        Color::Rgb(120, 220, 80),
        Color::Rgb(80, 220, 200),
        Color::Rgb(100, 160, 255),
        Color::Rgb(180, 100, 255),
        Color::Rgb(255, 100, 200),
    ],
// Pastel
[
    Color::Rgb(255, 180, 180),
    Color::Rgb(255, 210, 160),
    Color::Rgb(255, 245, 180),
    Color::Rgb(200, 240, 180),
    Color::Rgb(180, 240, 230),
    Color::Rgb(200, 220, 255),
    Color::Rgb(220, 190, 255),
    Color::Rgb(255, 200, 230),
],
// Mid
[
    Color::Rgb(200, 60, 60),
    Color::Rgb(200, 120, 40),
    Color::Rgb(200, 180, 50),
    Color::Rgb(90, 170, 60),
    Color::Rgb(60, 170, 160),
    Color::Rgb(70, 120, 200),
    Color::Rgb(140, 80, 200),
    Color::Rgb(200, 80, 150),
],
// Dark
[
    Color::Rgb(100, 30, 30),
    Color::Rgb(100, 60, 20),
    Color::Rgb(100, 90, 25),
    Color::Rgb(45, 85, 30),
    Color::Rgb(30, 85, 80),
    Color::Rgb(35, 60, 100),
    Color::Rgb(70, 40, 100),
    Color::Rgb(100, 40, 75),
],
// Grays
[
    Color::Rgb(20, 20, 20),
    Color::Rgb(50, 50, 50),
    Color::Rgb(80, 80, 80),
    Color::Rgb(120, 120, 120),
    Color::Rgb(160, 160, 160),
    Color::Rgb(200, 200, 200),
    Color::Rgb(230, 230, 230),
    Color::Rgb(255, 255, 255),
],
];

// ── draw ──────────────────────────────────────────────────────────────

pub fn draw(f: &mut Frame<'_>, area: Rect, ui: &ConfigUi, settings: &Settings) {
    let w = 76u16.min(area.width.saturating_sub(4));
    let h = 30u16.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;

    // FIXED: Biased vertical centering (60% top, 40% bottom)
    let y = area.y + (area.height.saturating_sub(h) * 6) / 10;
    let y = y.min(area.y + area.height.saturating_sub(h));

    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);

    match ui.mode {
        Mode::List => draw_list(f, rect, ui, settings),
        Mode::Colors => draw_colors(f, rect, ui, settings),
        Mode::Picker => draw_picker(f, rect, ui, settings),
    }
}

fn draw_list(f: &mut Frame<'_>, rect: Rect, ui: &ConfigUi, settings: &Settings) {
    let theme = &settings.theme;
    let bg = theme.color(Token::HelpBg);

    let title = if ui.dirty {
        " Settings · modified "
    } else {
        " Settings "
    };
    let block = Block::default()
    .borders(Borders::ALL)
    .border_set(theme.border_set())
    .border_style(Style::default().fg(Color::Cyan)) // Tied to the ⇅ icon
    .title(title)
    .style(Style::default().bg(bg));

    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let key_style = Style::default()
    .fg(Color::Cyan)
    .bg(bg)
    .add_modifier(Modifier::BOLD);
    let val_style = Style::default().fg(theme.color(Token::HelpFg)).bg(bg);
    let cursor_style = Style::default()
    .fg(bg)
    .bg(Color::Cyan)
    .add_modifier(Modifier::BOLD);
    let header_style = Style::default()
    .fg(theme.color(Token::HelpDim))
    .bg(bg)
    .add_modifier(Modifier::ITALIC);
    let dim_style = Style::default().fg(theme.color(Token::HelpDim)).bg(bg);

    let view_h = inner.height.saturating_sub(2) as usize;
    let scroll = if ui.cursor >= view_h {
        ui.cursor + 1 - view_h
    } else {
        0
    };

    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in ROWS.iter().enumerate().skip(scroll) {
        if lines.len() >= view_h {
            break;
        }
        let selected = i == ui.cursor;
        lines.push(render_row(
            row,
            settings,
            selected,
            key_style,
            val_style,
            cursor_style,
            header_style,
        ));
    }

    let footer = Line::from(vec![
        Span::styled(" ↑↓ select  ←→ adjust  ⏎ edit  ", dim_style),
                            Span::styled("u", key_style),
                            Span::styled(" undo  ", dim_style),
                            Span::styled("r", key_style),
                            Span::styled(" revert  ", dim_style),
                            Span::styled("s", key_style),
                            Span::styled(" save  ", dim_style),
                            Span::styled("esc", key_style),
                            Span::styled(" close", dim_style),
    ]);

    let mut all = lines;
    all.push(Line::from(""));
    all.push(footer);
    f.render_widget(
        Paragraph::new(all).style(Style::default().bg(bg)),
                    inner,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    row: &Row,
    settings: &Settings,
    selected: bool,
    key_style: Style,
    val_style: Style,
    cursor_style: Style,
    header_style: Style,
) -> Line<'static> {
    let (name, value): (String, String) = match row {
        Row::Header(s) => {
            return Line::from(Span::styled(format!("  ── {} ──", s), header_style));
        }
        Row::Enum(name, opts) => {
            let cur = settings.view_mode.as_deref().unwrap_or("auto");
            let idx = opts.iter().position(|o| *o == cur).unwrap_or(0);
            (name.to_string(), format!("◂ {} ▸", opts[idx]))
        }
        Row::Border => (
            "border".to_string(),
                        format!("◂ {} ▸", border_name(settings.theme.border)),
        ),
        Row::Bool(name) => {
            let on = match *name {
                "accent_number" => settings.labels.accent_number,
                "accent_fn" => settings.labels.accent_fn,
                "accent_instance" => settings.labels.accent_instance,
                "enabled" => settings.synesthesia.enabled,
                _ => false,
            };
            (
                name.to_string(),
             if on { "▣ yes".into() } else { "▢ no".into() },
            )
        }
        Row::Int(name, _, _) => {
            let v = match *name {
                "spiral_aspect_x" => settings.layout.spiral_aspect_x,
                "spiral_max_h" => settings.layout.spiral_max_h,
                "min_col_w" => settings.layout.min_col_w,
                "max_col_w" => settings.layout.max_col_w,
                "lookback_lines" => settings.layout.lookback_lines,
                _ => 0,
            };
            (name.to_string(), v.to_string())
        }
        Row::Float(name, _, _, _) => {
            let v = match *name {
                "cover_dim" => settings.layout.cover_dim as f64,
                "hue_step" => settings.synesthesia.hue_step,
                "saturation_scale" => settings.synesthesia.saturation_scale,
                "value" => settings.synesthesia.value,
                _ => 0.0,
            };
            (name.to_string(), format!("{:.2}", v))
        }
        Row::Glyph(name, _) => {
            let v = match *name {
                "playhead_marker" => settings.layout.playhead_marker.clone(),
                "help_glyph" => settings.layout.help_glyph.clone(),
                _ => "?".into(),
            };
            (name.to_string(), format!("◂ {} ▸", v))
        }
        Row::Overrides => (
            "overrides".to_string(),
                           format!("{} token(s)  ⏎ edit", count_overrides(settings)),
        ),
    };

    let marker = if selected { "▸ " } else { "  " };
    let label_style = if selected { cursor_style } else { key_style };
    Line::from(vec![
        Span::styled(format!("{} {:<16}", marker, name), label_style),
               Span::styled(value, val_style),
    ])
}

fn draw_colors(f: &mut Frame<'_>, rect: Rect, ui: &ConfigUi, settings: &Settings) {
    let theme = &settings.theme;
    let bg = theme.color(Token::HelpBg);

    let block = Block::default()
    .borders(Borders::ALL)
    .border_set(theme.border_set())
    .border_style(Style::default().fg(Color::Cyan))
    .title(" Colors ")
    .style(Style::default().bg(bg));

    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let key_style = Style::default()
    .fg(Color::Cyan)
    .bg(bg)
    .add_modifier(Modifier::BOLD);
    let val_style = Style::default().fg(theme.color(Token::HelpFg)).bg(bg);
    let cursor_style = Style::default()
    .fg(bg)
    .bg(Color::Cyan)
    .add_modifier(Modifier::BOLD);
    let dim_style = Style::default().fg(theme.color(Token::HelpDim)).bg(bg);

    let view_h = inner.height.saturating_sub(2) as usize;
    let scroll = if ui.color_cursor >= view_h {
        ui.color_cursor + 1 - view_h
    } else {
        0
    };

    let mut lines: Vec<Line> = Vec::new();
    for (i, &token) in Token::ALL.iter().enumerate().skip(scroll) {
        if lines.len() >= view_h {
            break;
        }
        let selected = i == ui.color_cursor;
        let name = token_name(token);
        let color = settings.theme.color(token);
        let swatch = Span::styled("███", Style::default().fg(color).bg(bg));
        let label_style = if selected { cursor_style } else { key_style };
        let marker = if selected { "▸ " } else { "  " };

        lines.push(Line::from(vec![
            Span::styled(format!("{} {:<22} ", marker, name), label_style),
                              swatch,
                              Span::styled(format!("  {}", fmt_color(color)), val_style),
        ]));
    }

    let footer = Line::from(vec![
        Span::styled(" ↑↓ select  ", dim_style),
                            Span::styled("⏎", key_style),
                            Span::styled(" pick  ", dim_style),
                            Span::styled("esc", key_style),
                            Span::styled(" back", dim_style),
    ]);

    let mut all = lines;
    all.push(Line::from(""));
    all.push(footer);
    f.render_widget(
        Paragraph::new(all).style(Style::default().bg(bg)),
                    inner,
    );
}

fn draw_picker(f: &mut Frame<'_>, rect: Rect, ui: &ConfigUi, settings: &Settings) {
    let theme = &settings.theme;
    let bg = theme.color(Token::HelpBg);

    let token = Token::ALL[ui.color_cursor];
    let current = settings.theme.color(token);
    let (cx, cy) = ui.picker_cursor;

    let block = Block::default()
    .borders(Borders::ALL)
    .border_set(theme.border_set())
    .border_style(Style::default().fg(Color::Cyan))
    .title(format!(" Pick: {} ", token_name(token)))
    .style(Style::default().bg(bg));

    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let val_style = Style::default().fg(theme.color(Token::HelpFg)).bg(bg);
    let dim_style = Style::default().fg(theme.color(Token::HelpDim)).bg(bg);
    let key_style = Style::default()
    .fg(Color::Cyan)
    .bg(bg)
    .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" current  ", dim_style),
                          Span::styled("███", Style::default().fg(current).bg(bg)),
                          Span::styled(format!("  {}", fmt_color(current)), val_style),
    ]));
    lines.push(Line::from(""));

    for row in 0..5 {
        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::raw(" "));
        for col in 0..8 {
            let color = PALETTE[row][col];
            let sel = row == cy && col == cx;
            if sel {
                spans.push(Span::styled("[", Style::default().fg(color).bg(bg)));
                spans.push(Span::styled("███", Style::default().fg(color).bg(bg)));
                spans.push(Span::styled("]", Style::default().fg(color).bg(bg)));
            } else {
                spans.push(Span::styled(" ███ ", Style::default().fg(color).bg(bg)));
            }
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" ←→↑↓ move  ", dim_style),
                          Span::styled("⏎", key_style),
                          Span::styled(" pick  ", dim_style),
                          Span::styled("esc", key_style),
                          Span::styled(" back", dim_style),
    ]));

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(bg)),
                    inner,
    );
}

// ── key handling ──────────────────────────────────────────────────────

pub fn handle_key(
    code: KeyCode,
    _mods: KeyModifiers,
    ui: &mut ConfigUi,
    settings: &mut Settings,
) -> Action {
    match ui.mode {
        Mode::List => handle_list_key(code, ui, settings),
        Mode::Colors => handle_colors_key(code, ui),
        Mode::Picker => handle_picker_key(code, ui, settings),
    }
}

fn handle_list_key(code: KeyCode, ui: &mut ConfigUi, settings: &mut Settings) -> Action {
    match code {
        KeyCode::Esc | KeyCode::Char('q') => return Action::Close,
        KeyCode::Up => {
            let mut c = ui.cursor;
            while c > 0 {
                c -= 1;
                if is_editable(ROWS.get(c)) {
                    ui.cursor = c;
                    break;
                }
            }
        }
        KeyCode::Down => {
            let max = ROWS.len().saturating_sub(1);
            let mut c = ui.cursor;
            while c < max {
                c += 1;
                if is_editable(ROWS.get(c)) {
                    ui.cursor = c;
                    break;
                }
            }
        }
        KeyCode::Left | KeyCode::Right => {
            let dir: i32 = if code == KeyCode::Left { -1 } else { 1 };
            if let Some(row) = ROWS.get(ui.cursor).copied() {
                ui.undo.push(settings.clone());
                ui.dirty = true;
                adjust(row, settings, dir);
            }
        }
        KeyCode::Enter => {
            if let Some(Row::Overrides) = ROWS.get(ui.cursor) {
                ui.mode = Mode::Colors;
                ui.color_cursor = 0;
            }
        }
        KeyCode::Char('u') => {
            if let Some(prev) = ui.undo.pop() {
                *settings = prev;
                ui.dirty = true;
            }
        }
        KeyCode::Char('r') => {
            if let Ok(cfg) = reload_config() {
                *settings = cfg;
                ui.undo.clear();
                ui.undo.push(settings.clone());
                ui.dirty = false;
            }
        }
        KeyCode::Char('s') => {
            if let Err(e) = save_settings(settings) {
                eprintln!("save failed: {:#}", e);
            } else {
                ui.dirty = false;
            }
        }
        _ => {}
    }
    Action::Continue
}

fn handle_colors_key(code: KeyCode, ui: &mut ConfigUi) -> Action {
    match code {
        KeyCode::Esc | KeyCode::Char('q') => ui.mode = Mode::List,
        KeyCode::Up => ui.color_cursor = ui.color_cursor.saturating_sub(1),
        KeyCode::Down => {
            ui.color_cursor = (ui.color_cursor + 1).min(Token::ALL.len() - 1);
        }
        KeyCode::Enter => {
            ui.mode = Mode::Picker;
            ui.picker_cursor = (0, 0);
        }
        _ => {}
    }
    Action::Continue
}

fn handle_picker_key(
    code: KeyCode,
    ui: &mut ConfigUi,
    settings: &mut Settings,
) -> Action {
    let (cx, cy) = ui.picker_cursor;
    match code {
        KeyCode::Esc | KeyCode::Char('q') => ui.mode = Mode::Colors,
        KeyCode::Left => ui.picker_cursor = (cx.saturating_sub(1), cy),
        KeyCode::Right => ui.picker_cursor = ((cx + 1).min(7), cy),
        KeyCode::Up => ui.picker_cursor = (cx, cy.saturating_sub(1)),
        KeyCode::Down => ui.picker_cursor = (cx, (cy + 1).min(4)),
        KeyCode::Enter => {
            let color = PALETTE[cy][cx];
            ui.undo.push(settings.clone());
            ui.dirty = true;
            let token = Token::ALL[ui.color_cursor];
            settings.theme.set(token, color);
            ui.mode = Mode::Colors;
        }
        _ => {}
    }
    Action::Continue
}

fn adjust(row: Row, settings: &mut Settings, dir: i32) {
    match row {
        Row::Enum(_, opts) => {
            let cur = settings.view_mode.as_deref().unwrap_or("auto");
            let idx = opts.iter().position(|o| *o == cur).unwrap_or(0) as i32;
            let next = (idx + dir).rem_euclid(opts.len() as i32) as usize;
            settings.view_mode = match opts[next] {
                "auto" => None,
                other => Some(other.to_string()),
            };
        }
        Row::Border => {
            settings.theme.border = next_border(settings.theme.border, dir);
        }
        Row::Bool(name) => match name {
            "accent_number" => {
                settings.labels.accent_number = !settings.labels.accent_number
            }
            "accent_fn" => settings.labels.accent_fn = !settings.labels.accent_fn,
            "accent_instance" => {
                settings.labels.accent_instance = !settings.labels.accent_instance
            }
            "enabled" => settings.synesthesia.enabled = !settings.synesthesia.enabled,
            _ => {}
        },
        Row::Int(name, lo, hi) => {
            let step = dir as i64;
            match name {
                "spiral_aspect_x" => {
                    settings.layout.spiral_aspect_x =
                    (settings.layout.spiral_aspect_x as i64 + step).clamp(lo, hi) as usize
                }
                "spiral_max_h" => {
                    settings.layout.spiral_max_h =
                    (settings.layout.spiral_max_h as i64 + step).clamp(lo, hi) as usize
                }
                "min_col_w" => {
                    settings.layout.min_col_w =
                    (settings.layout.min_col_w as i64 + step).clamp(lo, hi) as usize
                }
                "max_col_w" => {
                    settings.layout.max_col_w =
                    (settings.layout.max_col_w as i64 + step).clamp(lo, hi) as usize
                }
                "lookback_lines" => {
                    settings.layout.lookback_lines =
                    (settings.layout.lookback_lines as i64 + step * 4).clamp(lo, hi)
                    as usize
                }
                _ => {}
            }
        }
        Row::Float(name, lo, hi, step) => {
            let d = dir as f64 * step;
            match name {
                "cover_dim" => {
                    settings.layout.cover_dim =
                    ((settings.layout.cover_dim as f64 + d).clamp(lo, hi)) as f32
                }
                "hue_step" => {
                    settings.synesthesia.hue_step =
                    (settings.synesthesia.hue_step + d).clamp(lo, hi)
                }
                "saturation_scale" => {
                    settings.synesthesia.saturation_scale =
                    (settings.synesthesia.saturation_scale + d).clamp(lo, hi)
                }
                "value" => {
                    settings.synesthesia.value =
                    (settings.synesthesia.value + d).clamp(lo, hi)
                }
                _ => {}
            }
        }
        Row::Glyph(name, opts) => {
            let cur = match name {
                "playhead_marker" => &settings.layout.playhead_marker,
                "help_glyph" => &settings.layout.help_glyph,
                _ => return,
            };
            let idx = opts.iter().position(|o| o == cur).unwrap_or(0) as i32;
            let next = (idx + dir).rem_euclid(opts.len() as i32) as usize;
            match name {
                "playhead_marker" => {
                    settings.layout.playhead_marker = opts[next].to_string()
                }
                "help_glyph" => settings.layout.help_glyph = opts[next].to_string(),
                _ => {}
            }
        }
        Row::Header(_) | Row::Overrides => {}
    }
}

fn next_border(b: BorderStyle, dir: i32) -> BorderStyle {
    const ALL: &[BorderStyle] = &[
        BorderStyle::Plain,
        BorderStyle::Double,
        BorderStyle::Mixed,
        BorderStyle::Rounded,
        BorderStyle::Thick,
    ];
    let idx = ALL.iter().position(|x| *x == b).unwrap_or(0) as i32;
    ALL[(idx + dir).rem_euclid(ALL.len() as i32) as usize]
}

// ── helpers ───────────────────────────────────────────────────────────

fn token_name(t: Token) -> String {
    serde_json::to_value(t)
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| format!("{:?}", t))
}

fn border_name(b: BorderStyle) -> &'static str {
    match b {
        BorderStyle::Plain => "plain",
        BorderStyle::Double => "double",
        BorderStyle::Mixed => "mixed",
        BorderStyle::Rounded => "rounded",
        BorderStyle::Thick => "thick",
    }
}

fn count_overrides(settings: &Settings) -> usize {
    let base = builtin::default_colors();
    Token::ALL
    .iter()
    .filter(|t| {
        let cur = settings.theme.color(**t);
        let def = base.get(*t).copied().unwrap_or(Color::Reset);
        cur != def
    })
    .count()
}

fn fmt_color(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{:02x}{:02x}{:02x}", r, g, b),
        Color::Black => "black".into(),
        Color::Red => "red".into(),
        Color::Green => "green".into(),
        Color::Yellow => "yellow".into(),
        Color::Blue => "blue".into(),
        Color::Magenta => "magenta".into(),
        Color::Cyan => "cyan".into(),
        Color::Gray => "gray".into(),
        Color::DarkGray => "darkgray".into(),
        Color::White => "white".into(),
        Color::Reset => "reset".into(),
        other => format!("{:?}", other),
    }
}

fn reload_config() -> Result<Settings> {
    Ok(Config::load_layered(None).resolve())
}

fn save_settings(settings: &Settings) -> Result<()> {
    let path = paths::user_config().join("player_settings.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Preserve any existing preset name; this UI doesn't edit it.
    let preset = std::fs::read_to_string(&path)
    .ok()
    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    .and_then(|v| v.get("theme").and_then(|t| t.get("preset")).cloned())
    .unwrap_or(serde_json::Value::Null);

    let mut base = builtin::default_colors();
    if let Some(name) = preset.as_str() {
        let _ = builtin::apply_preset(name, &mut base);
    }

    let mut overrides = serde_json::Map::new();
    for &t in Token::ALL {
        let cur = settings.theme.color(t);
        let def = base.get(&t).copied().unwrap_or(Color::Reset);
        if cur != def {
            overrides.insert(token_name(t), serde_json::json!(fmt_color(cur)));
        }
    }

    let json_root = serde_json::json!({
        "theme": {
            "preset": preset,
            "border": border_name(settings.theme.border),
                                      "overrides": overrides,
        },
        "view": {
            "mode": settings.view_mode,
        },
        "layout": {
            "cover_dim": settings.layout.cover_dim,
            "spiral_aspect_x": settings.layout.spiral_aspect_x,
            "spiral_max_h": settings.layout.spiral_max_h,
            "min_col_w": settings.layout.min_col_w,
            "max_col_w": settings.layout.max_col_w,
            "lookback_lines": settings.layout.lookback_lines,
            "playhead_marker": settings.layout.playhead_marker,
            "help_glyph": settings.layout.help_glyph,
        },
        "labels": {
            "accent_number": settings.labels.accent_number,
            "accent_fn": settings.labels.accent_fn,
            "accent_instance": settings.labels.accent_instance,
        },
        "synesthesia": {
            "enabled": settings.synesthesia.enabled,
            "hue_step": settings.synesthesia.hue_step,
            "saturation_from_volume": settings.synesthesia.saturation_from_volume,
            "saturation_scale": settings.synesthesia.saturation_scale,
            "value": settings.synesthesia.value,
        },
    });

    std::fs::write(&path, serde_json::to_string_pretty(&json_root)?)?;
    Ok(())
}
