//! Every color the UI can change.  Call sites ask for
//! `theme.color(Token::X)`; the JSON config overrides individual
//! tokens by name.  Glyph selection (border style) is a separate axis
//! — see `BorderStyle` in `theme/mod.rs`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Token {
    // Chrome
    Border,
    Playhead,

    // Spiral
    SpiralCurrent,
    SpiralPlayed,
    SpiralFuture,
    SpiralLoopBg,

    // Part / note state
    MutedFg,
    MutedBg,
    Soloed,
    HelpButton,
    BlockNumber,
    BlockNumberPlaying,
    BlockFn,
    BlockInstance,

    // Note lifecycle
    NotePlayingFg,
    NotePlayingBg,
    Played,
    Unplayed,
    Wave,

    // Help overlay
    HelpFg,
    HelpKey,
    HelpDim,
    HelpBg,

    // Queue box
    QueueBg,
    QueueTransportActive,
    QueueTransportIdle,
    QueueText,
    QueuePlaying,
    QueueCounter,
    QueueCounterDim,
    QueueCursorFg,
    QueueCursorBg,
}

impl Token {
    /// Every token, in the order used by `builtin::default_colors`.
    /// Adding a token requires adding it here and to that map; the
    /// compiler will catch one, `Theme::from_parts` will catch the
    /// other at startup.
    pub const ALL: &'static [Token] = &[
        Token::Border,
        Token::Playhead,
        Token::SpiralCurrent,
        Token::SpiralPlayed,
        Token::SpiralFuture,
        Token::SpiralLoopBg,
        Token::MutedFg,
        Token::MutedBg,
        Token::Soloed,
        Token::HelpButton,
        Token::BlockNumber,
        Token::BlockNumberPlaying,
        Token::BlockFn,
        Token::BlockInstance,
        Token::NotePlayingFg,
        Token::NotePlayingBg,
        Token::Played,
        Token::Unplayed,
        Token::Wave,
        Token::HelpFg,
        Token::HelpKey,
        Token::HelpDim,
        Token::HelpBg,
        Token::QueueBg,
        Token::QueueTransportActive,
        Token::QueueTransportIdle,
        Token::QueueText,
        Token::QueuePlaying,
        Token::QueueCounter,
        Token::QueueCounterDim,
        Token::QueueCursorFg,
        Token::QueueCursorBg,
    ];
}