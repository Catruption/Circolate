//! `share/player_settings.json`.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::tui::theme::{builtin, parse_color, BorderStyle, Theme, Token};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub theme: ThemeConfig,
    #[serde(default)]
    pub view: ViewConfig,
    #[serde(default)]
    pub layout: LayoutConfig,
    #[serde(default)]
    pub labels: LabelsConfig,
    #[serde(default)]
    pub synesthesia: SynesthesiaConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub border: Option<BorderStyle>,
    #[serde(default)]
    pub overrides: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewConfig {
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutConfig {
    #[serde(default = "d_cover_dim")]       pub cover_dim: f32,
    #[serde(default = "d_min_col_w")]       pub min_col_w: usize,
    #[serde(default = "d_max_col_w")]       pub max_col_w: usize,
    #[serde(default = "d_min_wave_rows")]   pub min_wave_rows: usize,
    #[serde(default = "d_max_wave_rows")]   pub max_wave_rows: usize,
    #[serde(default = "d_part_gap")]        pub part_gap: usize,
    #[serde(default = "d_max_event_lines")] pub max_event_lines: usize,
    #[serde(default = "d_lookback_lines")]  pub lookback_lines: usize,
    #[serde(default = "d_playhead_marker")] pub playhead_marker: String,
    #[serde(default = "d_spiral_aspect_x")] pub spiral_aspect_x: usize,
    #[serde(default = "d_spiral_max_h")]    pub spiral_max_h: usize,
    #[serde(default = "d_help_glyph")]      pub help_glyph: String,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            cover_dim: d_cover_dim(),
            min_col_w: d_min_col_w(),
            max_col_w: d_max_col_w(),
            min_wave_rows: d_min_wave_rows(),
            max_wave_rows: d_max_wave_rows(),
            part_gap: d_part_gap(),
            max_event_lines: d_max_event_lines(),
            lookback_lines: d_lookback_lines(),
            playhead_marker: d_playhead_marker(),
            spiral_aspect_x: d_spiral_aspect_x(),
            spiral_max_h: d_spiral_max_h(),
            help_glyph: d_help_glyph(),
        }
    }
}

fn d_cover_dim() -> f32 { 0.4 }
fn d_min_col_w() -> usize { 10 }
fn d_max_col_w() -> usize { 40 }
fn d_min_wave_rows() -> usize { 2 }
fn d_max_wave_rows() -> usize { 10 }
fn d_part_gap() -> usize { 1 }
fn d_max_event_lines() -> usize { 8 }
fn d_lookback_lines() -> usize { 128 }
fn d_playhead_marker() -> String { "━".into() }
fn d_spiral_aspect_x() -> usize { 2 }
fn d_spiral_max_h() -> usize { 15 }
fn d_help_glyph() -> String { "?".into() }

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelsConfig {
    #[serde(default = "d_true")] pub accent_number: bool,
    #[serde(default = "d_true")] pub accent_fn: bool,
    #[serde(default = "d_true")] pub accent_instance: bool,
}

impl Default for LabelsConfig {
    fn default() -> Self {
        Self { accent_number: true, accent_fn: true, accent_instance: true }
    }
}

fn d_true() -> bool { true }

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynesthesiaConfig {
    #[serde(default = "d_true")]            pub enabled: bool,
    #[serde(default = "d_hue_step")]        pub hue_step: f64,
    #[serde(default = "d_true")]            pub saturation_from_volume: bool,
    #[serde(default = "d_one")]             pub saturation_scale: f64,
    #[serde(default = "d_one")]             pub value: f64,
}

impl Default for SynesthesiaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hue_step: d_hue_step(),
            saturation_from_volume: true,
            saturation_scale: d_one(),
            value: d_one(),
        }
    }
}

fn d_hue_step() -> f64 { 30.0 }
fn d_one() -> f64 { 1.0 }

#[derive(Clone)]
pub struct Settings {
    pub theme: Theme,
    pub layout: LayoutConfig,
    pub labels: LabelsConfig,
    pub synesthesia: SynesthesiaConfig,
    pub view_mode: Option<String>,
}

impl Config {
    pub fn load_layered(trackscript_dir: Option<&Path>) -> Self {
        let mut cfg = Config::default();
        let sys = crate::paths::system_data().join("player_settings.json");
        cfg.merge_from_file(&sys);
        let user = crate::paths::user_config().join("player_settings.json");
        cfg.merge_from_file(&user);
        if let Some(dir) = trackscript_dir {
            cfg.merge_from_file(&dir.join("player_settings.json"));
        }
        cfg
    }

    fn merge_from_file(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else { return };
        match serde_json::from_str::<Config>(&text) {
            Ok(next) => self.merge(next),
            Err(e) => crate::log_event!(
                "tui.settings.parse_failed",
                path: path.display(),
                                        err: e,
            ),
        }
    }

    fn merge(&mut self, other: Config) {
        if other.preset.is_some() { self.preset = other.preset; }
        if other.theme.preset.is_some() { self.theme.preset = other.theme.preset; }
        if other.theme.border.is_some() { self.theme.border = other.theme.border; }
        self.theme.overrides.extend(other.theme.overrides);
        if other.view.mode.is_some() { self.view.mode = other.view.mode; }
        self.layout = other.layout;
        self.labels = other.labels;
        self.synesthesia = other.synesthesia;
    }

    pub fn resolve(self) -> Settings {
        Settings {
            theme: self.resolve_theme(),
            layout: normalize_layout(self.layout),
            labels: self.labels,
            synesthesia: self.synesthesia,
            view_mode: self.view.mode,
        }
    }

    fn resolve_theme(&self) -> Theme {
        let mut colors = builtin::default_colors();
        let mut border = self.theme.border.unwrap_or_default();

        if let Some(name) = self.preset.as_deref() {
            if let Err(e) = builtin::apply_preset(name, &mut colors) {
                crate::log_event!("tui.settings.preset_failed", err: e);
            }
        }
        if let Some(name) = self.theme.preset.as_deref() {
            if let Err(e) = builtin::apply_preset(name, &mut colors) {
                crate::log_event!("tui.settings.preset_failed", err: e);
            }
        }
        for (k, v) in &self.theme.overrides {
            let Some(tok) = token_from_name(k) else {
                crate::log_event!("tui.settings.unknown_token", token: k.clone());
                continue;
            };
            let Some(col) = parse_color(v) else {
                crate::log_event!(
                    "tui.settings.bad_color",
                    value: v.clone(),
                                  token: k.clone(),
                );
                continue;
            };
            colors.insert(tok, col);
        }
        if let Some(b) = self.theme.border { border = b; }
        Theme::from_parts(colors, border)
    }
}

fn normalize_layout(mut l: LayoutConfig) -> LayoutConfig {
    l.cover_dim = l.cover_dim.clamp(0.0, 2.0);
    if l.min_col_w < 4 { l.min_col_w = 4; }
    if l.max_col_w < l.min_col_w { l.max_col_w = l.min_col_w; }
    if l.min_wave_rows < 1 { l.min_wave_rows = 1; }
    if l.max_wave_rows < l.min_wave_rows { l.max_wave_rows = l.min_wave_rows; }
    if l.spiral_aspect_x < 1 { l.spiral_aspect_x = 1; }
    if l.spiral_max_h < 2 { l.spiral_max_h = 2; }
    if l.help_glyph.is_empty() { l.help_glyph = "?".into(); }
    if l.playhead_marker.is_empty() { l.playhead_marker = " ".into(); }
    l
}

fn token_from_name(s: &str) -> Option<Token> {
    Some(match s {
        "border" => Token::Border,
         "playhead" => Token::Playhead,
         "spiral-current" => Token::SpiralCurrent,
         "spiral-played" => Token::SpiralPlayed,
         "spiral-future" => Token::SpiralFuture,
         "spiral-loop-bg" => Token::SpiralLoopBg,
         "muted-fg" => Token::MutedFg,
         "muted-bg" => Token::MutedBg,
         "soloed" => Token::Soloed,
         "help-button" => Token::HelpButton,
         "block-number" => Token::BlockNumber,
         "block-number-playing" => Token::BlockNumberPlaying,
         "block-fn" => Token::BlockFn,
         "block-instance" => Token::BlockInstance,
         "note-playing-fg" => Token::NotePlayingFg,
         "note-playing-bg" => Token::NotePlayingBg,
         "played" => Token::Played,
         "unplayed" => Token::Unplayed,
         "wave" => Token::Wave,
         "help-fg" => Token::HelpFg,
         "help-key" => Token::HelpKey,
         "help-dim" => Token::HelpDim,
         "help-bg" => Token::HelpBg,
         "queue-bg" => Token::QueueBg,
         "queue-transport-active" => Token::QueueTransportActive,
         "queue-transport-idle" => Token::QueueTransportIdle,
         "queue-text" => Token::QueueText,
         "queue-playing" => Token::QueuePlaying,
         "queue-counter" => Token::QueueCounter,
         "queue-counter-dim" => Token::QueueCounterDim,
         "queue-cursor-fg" => Token::QueueCursorFg,
         "queue-cursor-bg" => Token::QueueCursorBg,
         _ => return None,
    })
}
