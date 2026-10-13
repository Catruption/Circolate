//! Box-drawing glyph selection and UI colors.  Change `BORDER_STYLE` to
//! switch every widget's border characters — and the spiral playbar's
//! path glyphs — in one place.

use ratatui::{
    symbols::border,
    style::{Color, Style},
    widgets::{Block, Borders},
};

/// Which box-drawing character set to use for widget borders and the
/// spiral playbar.
#[derive(Clone, Copy)]
pub enum BorderStyle {
    /// Plain single-line: ┌─┐ / │ │ / └─┘
    Plain,
    /// Full double-line: ╔═╗ / ║ ║ / ╚═╝
    Double,
    /// Mixed: double verticals, single horizontals — ╓─╖ / ║ ║ / ╙─╜
    Mixed,
    /// Rounded single-line: ╭─╮ / │ │ / ╰─╯
    Rounded,
    /// Thick single-line: ┏━┓ / ┃ ┃ / ┗━┛
    Thick,
}

/// The border style used throughout the UI.  Change this one value to
/// switch every widget's box-drawing glyphs, including the spiral
/// playbar's path.
pub const BORDER_STYLE: BorderStyle = BorderStyle::Double;

/// Foreground color for all border glyphs.
const BORDER_FG: Option<Color> = Some(Color::White);

/// Color for the sample-position playhead: the moving cursor that walks
/// down a column as the underlying sample plays through.  Applied as a
/// foreground change only, so the note's pitch-hue background and the
/// mosaic cover pixel both stay visible.
pub const PLAYHEAD_FG: Color = Color::Yellow;

/// Glyph drawn on empty cells under the playhead so the cursor stays
/// visible even where no note or waveform is drawn.
pub const PLAYHEAD_MARKER: &str = "━";

// ── spiral playbar colors ──────────────────────────────────────────────

/// Currently-playing measure (or measure bundle).
pub const SPIRAL_CURRENT_FG: Color = Color::Yellow;
/// Measures that have already played.
pub const SPIRAL_PLAYED_FG: Color = Color::DarkGray;
/// Measures that haven't been reached yet.
pub const SPIRAL_FUTURE_FG: Color = Color::Rgb(120, 120, 120);
/// Background tint for measures that are in the loop set.
pub const SPIRAL_LOOP_BG: Color = Color::Rgb(60, 50, 10);

// ── mute / solo display ────────────────────────────────────────────────

/// Foreground for a muted note or part header.  Fully black so muted
/// cells read as "off".
pub const MUTED_FG: Color = Color::Black;

/// Background for muted cells.  Explicit so the mosaic pass doesn't
/// paint a cover pixel behind the text — black-on-dark-cover would
/// otherwise be invisible in exactly the places the mosaic is darkest.
/// A medium grey reads as "greyed out" rather than "hole in the UI".
pub const MUTED_BG: Color = Color::Rgb(60, 60, 60);

/// Header foreground for a soloed part.
pub const SOLOED_FG: Color = Color::Cyan;
/// Header foreground for the focused column.
pub const FOCUSED_FG: Color = Color::Yellow;

// ── help overlay ───────────────────────────────────────────────────────

pub const HELP_FG: Color = Color::White;
pub const HELP_KEY_FG: Color = Color::Yellow;
pub const HELP_DIM_FG: Color = Color::DarkGray;
pub const HELP_BG: Color = Color::Rgb(20, 20, 30);

pub fn border_set(style: BorderStyle) -> border::Set {
    match style {
        BorderStyle::Plain => border::PLAIN,
        BorderStyle::Double => border::DOUBLE,
        BorderStyle::Mixed => border::Set {
            top_left: "╓",
            top_right: "╖",
            bottom_left: "╙",
            bottom_right: "╜",
            horizontal_top: "─",
            horizontal_bottom: "─",
            vertical_left: "║",
            vertical_right: "║",
        },
        BorderStyle::Rounded => border::ROUNDED,
        BorderStyle::Thick => border::THICK,
    }
}

fn border_style() -> Style {
    match BORDER_FG {
        Some(c) => Style::default().fg(c),
        None => Style::default(),
    }
}

/// A block with all four borders, using the currently-selected glyph
/// set and the explicit border foreground.  Callers still chain
/// `.title()` and any other block options they need.
pub fn block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_set(border_set(BORDER_STYLE))
        .border_style(border_style())
}

/// Directions a path cell connects in.  Used by the spiral playbar to
/// pick the right box-drawing glyph for each cell of the path.
#[derive(Clone, Copy, Default)]
pub struct PathDirs {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
}

/// Pick the box-drawing glyph for a path cell connected in the given
/// directions, using the current border style.  Endpoint cells (only
/// one connection) render as a straight segment in the direction they
/// connect.
pub fn path_glyph(dirs: PathDirs, style: BorderStyle) -> &'static str {
    let set = border_set(style);
    match (dirs.left, dirs.right, dirs.up, dirs.down) {
        (true, true, false, false) => set.horizontal_top,
        (false, false, true, true) => set.vertical_left,
        (false, true, false, true) => set.top_left,
        (true, false, false, true) => set.top_right,
        (false, true, true, false) => set.bottom_left,
        (true, false, true, false) => set.bottom_right,
        (true, false, false, false) | (false, true, false, false) => set.horizontal_top,
        (false, false, true, false) | (false, false, false, true) => set.vertical_left,
        _ => " ",
    }
}