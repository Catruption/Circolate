//! Built-in theme defaults and named presets.  The default map must
//! cover every `Token`; `Theme::from_parts` fills any gaps, but they
//! should never appear in practice.

use std::collections::HashMap;

use ratatui::style::Color;

use super::Token;

pub fn default_colors() -> HashMap<Token, Color> {
    use Color::*;
    use Token::*;

    let mut m = HashMap::new();

    m.insert(Border, White);
    m.insert(Playhead, Yellow);

    m.insert(SpiralCurrent, Yellow);
    m.insert(SpiralPlayed, DarkGray);
    m.insert(SpiralFuture, Rgb(120, 120, 120));
    m.insert(SpiralLoopBg, Rgb(60, 50, 10));

    m.insert(MutedFg, Black);
    m.insert(MutedBg, Rgb(60, 60, 60));
    m.insert(Soloed, Cyan);
    m.insert(HelpButton, Yellow);
    m.insert(BlockNumber, Rgb(180, 220, 255));
    m.insert(BlockNumberPlaying, Rgb(255, 235, 100));
    m.insert(BlockFn, Rgb(255, 200, 120));
    m.insert(BlockInstance, Rgb(140, 180, 210));

    m.insert(NotePlayingFg, White);
    m.insert(NotePlayingBg, Rgb(120, 120, 120));
    m.insert(Played, DarkGray);
    m.insert(Unplayed, White);
    m.insert(Wave, Rgb(200, 200, 200));

    m.insert(HelpFg, White);
    m.insert(HelpKey, Yellow);
    m.insert(HelpDim, DarkGray);
    m.insert(HelpBg, Rgb(20, 20, 30));

    m.insert(QueueBg, Rgb(0, 0, 0));
    m.insert(QueueTransportActive, White);
    m.insert(QueueTransportIdle, DarkGray);
    m.insert(QueueText, Gray);
    m.insert(QueuePlaying, Cyan);
    m.insert(QueueCounter, Yellow);
    m.insert(QueueCounterDim, Rgb(70, 70, 70));
    m.insert(QueueCursorFg, Black);
    m.insert(QueueCursorBg, White);

    m
}

/// Per-token fallback for tokens a preset didn't set.  Not a substitute
/// for `default_colors`; a safety net.
pub fn fallback(t: Token) -> Color {
    default_colors().get(&t).copied().unwrap_or(Color::Reset)
}

/// Overlay a preset's colors on top of `base`.  Presets are sparse:
/// they only name the tokens they want to change.
pub fn apply_preset(name: &str, base: &mut HashMap<Token, Color>) -> Result<(), String> {
    use Color::*;
    use Token::*;

    match name {
        "default" => Ok(()),
        "gruvbox" => {
            base.insert(Border, Rgb(235, 219, 178));
            base.insert(Playhead, Rgb(250, 189, 47));
            base.insert(SpiralCurrent, Rgb(250, 189, 47));
            base.insert(SpiralPlayed, Rgb(80, 73, 69));
            base.insert(SpiralFuture, Rgb(146, 131, 116));
            base.insert(SpiralLoopBg, Rgb(60, 56, 54));
            base.insert(Soloed, Rgb(142, 192, 124));
            base.insert(HelpButton, Rgb(250, 189, 47));
            base.insert(BlockNumber, Rgb(131, 165, 152));
            base.insert(BlockNumberPlaying, Rgb(250, 189, 47));
            base.insert(BlockFn, Rgb(215, 153, 33));
            base.insert(BlockInstance, Rgb(104, 157, 106));
            base.insert(NotePlayingBg, Rgb(80, 73, 69));
            base.insert(Played, Rgb(80, 73, 69));
            base.insert(QueueBg, Rgb(29, 32, 33));
            base.insert(QueuePlaying, Rgb(184, 187, 38));
            base.insert(QueueCounter, Rgb(250, 189, 47));
            Ok(())
        }
        "synthwave" => {
            base.insert(Border, Rgb(255, 110, 199));
            base.insert(Playhead, Rgb(0, 255, 255));
            base.insert(SpiralCurrent, Rgb(255, 110, 199));
            base.insert(SpiralPlayed, Rgb(80, 40, 90));
            base.insert(SpiralFuture, Rgb(120, 60, 160));
            base.insert(SpiralLoopBg, Rgb(40, 10, 60));
            base.insert(Soloed, Rgb(0, 255, 255));
            base.insert(HelpButton, Rgb(255, 110, 199));
            base.insert(BlockNumber, Rgb(0, 255, 255));
            base.insert(BlockNumberPlaying, Rgb(255, 255, 0));
            base.insert(BlockFn, Rgb(255, 110, 199));
            base.insert(BlockInstance, Rgb(180, 130, 255));
            base.insert(NotePlayingBg, Rgb(60, 20, 80));
            base.insert(Played, Rgb(80, 60, 100));
            base.insert(QueueBg, Rgb(15, 8, 30));
            base.insert(QueuePlaying, Rgb(0, 255, 255));
            base.insert(QueueCounter, Rgb(255, 110, 199));
            Ok(())
        }
        other => Err(format!("unknown preset: {other}")),
    }
}