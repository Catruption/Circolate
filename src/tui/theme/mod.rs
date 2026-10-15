//! Runtime theme: a color per token, plus a border glyph set.  Built-in
//! defaults live in `builtin`; the JSON config overlays individual
//! tokens by name.  Call sites never hardcode a color — they ask for a
//! token.  See `tokens::Token::ALL` for the full set.

pub mod builtin;
pub mod tokens;

pub use tokens::Token;

use std::collections::HashMap;

use ratatui::{
    style::{Color, Style},
    symbols::border,
    widgets::{Block, Borders},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BorderStyle {
    Plain,
    #[default]
    Double,
    Mixed,
    Rounded,
    Thick,
}

#[derive(Clone, Debug)]
pub struct Theme {
    colors: HashMap<Token, Color>,
    pub border: BorderStyle,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            colors: builtin::default_colors(),
            border: BorderStyle::default(),
        }
    }
}

impl Theme {
    /// Construct from a full token map + border style.  Panics in debug
    /// if a token is missing — that means `builtin::default_colors`
    /// drifted from `Token::ALL`.
    pub fn from_parts(mut colors: HashMap<Token, Color>, border: BorderStyle) -> Self {
        for &t in Token::ALL {
            if !colors.contains_key(&t) {
                colors.insert(t, builtin::fallback(t));
            }
        }
        Self { colors, border }
    }

    pub fn color(&self, t: Token) -> Color {
        self.colors.get(&t).copied().unwrap_or(Color::Reset)
    }

    pub fn style(&self, t: Token) -> Style {
        Style::default().fg(self.color(t))
    }

    /// Apply a single override.  Used by the config loader.
    pub fn set(&mut self, t: Token, c: Color) {
        self.colors.insert(t, c);
    }

    // ── Glyph tables ────────────────────────────────────────────────
    // These aren't colors, so they aren't tokens; they're selected by
    // the `BorderStyle` axis instead.

    pub fn border_set(&self) -> border::Set {
        match self.border {
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

    pub fn block(&self) -> Block<'static> {
        Block::default()
        .borders(Borders::ALL)
        .border_set(self.border_set())
        .border_style(self.style(Token::Border))
    }
}

#[derive(Clone, Copy, Default)]
pub struct PathDirs {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
}

/// Pick the box-drawing glyph for a spiral cell with the given
/// incoming/outgoing directions.  Uses the current border set, so the
/// spiral's path shares the frame's glyphs.
pub fn path_glyph(dirs: PathDirs, style: BorderStyle) -> &'static str {
    let set = match style {
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
    };
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

/// Parse `#rgb`, `#rrggbb`, or one of ratatui's named colors.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
                let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
                let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
                Some(Color::Rgb(r, g, b))
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Color::Rgb(r, g, b))
            }
            _ => None,
        };
    }
    Some(match s.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
         "red" => Color::Red,
         "green" => Color::Green,
         "yellow" => Color::Yellow,
         "blue" => Color::Blue,
         "magenta" => Color::Magenta,
         "cyan" => Color::Cyan,
         "gray" | "grey" => Color::Gray,
         "darkgray" | "darkgrey" => Color::DarkGray,
         "white" => Color::White,
         "reset" => Color::Reset,
         _ => return None,
    })
}
