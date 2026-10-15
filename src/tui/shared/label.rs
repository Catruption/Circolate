//! Block label rendering.  A label like `4: ƒSawer(1)` gets its leading
//! number, its `ƒ` sigil, and its `(N)` instance index accented;
//! everything else inherits the caller's base style.  Which pieces get
//! accented is a user setting (`LabelsConfig`); the tokens they accent
//! into are themeable.
//!
//! Also the one place that decides precedence between soloed and muted
//! for a header, so both views agree.

use std::collections::HashSet;

use ratatui::{
    style::{Modifier, Style},
    text::Span,
};

use crate::tui::config::LabelsConfig;
use crate::tui::theme::{Theme, Token};

/// Split a block label into styled spans.
///
/// `playing` picks the number's token: `BlockNumberPlaying` when the
/// block has a note currently under the playhead, `BlockNumber` when
/// it's just being displayed (waiting, tailing out, or on a paused
/// transport).
pub fn split(
    label: &str,
    base: Style,
    theme: &Theme,
    cfg: &LabelsConfig,
    playing: bool,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = label.chars().collect();
    let mut i = 0;

    // Leading block number, e.g. `4:` in `4: ƒCh00`.
    if cfg.accent_number && i < chars.len() && chars[i].is_ascii_digit() {
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i < chars.len() && matches!(chars[i], ':' | '.' | ' ') {
            i += 1;
        }
        let num: String = chars[start..i].iter().collect();
        let num_token = if playing {
            Token::BlockNumberPlaying
        } else {
            Token::BlockNumber
        };
        spans.push(Span::styled(num, base.fg(theme.color(num_token))));
    }

    let mut buf = String::new();
    while i < chars.len() {
        let c = chars[i];

        // `ƒ` source-call sigil.
        if cfg.accent_fn && c == 'ƒ' {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), base));
            }
            spans.push(Span::styled(
                "ƒ".to_string(),
                base.fg(theme.color(Token::BlockFn)),
            ));
            i += 1;
            continue;
        }

        // `(N)` instance index — digits only, so `(saw)` and other
        // parenthesized prose isn't accidentally accented.
        if cfg.accent_instance && c == '(' {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > start && j < chars.len() && chars[j] == ')' {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), base));
                }
                let group: String = chars[i..=j].iter().collect();
                spans.push(Span::styled(
                    group,
                    base.fg(theme.color(Token::BlockInstance)),
                ));
                i = j + 1;
                continue;
            }
        }

        buf.push(c);
        i += 1;
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, base));
    }
    spans
}

/// Precedence for a header: soloed beats muted.  The two states are
/// exclusive per part, so at most one ever applies.
pub fn header_style(
    path: &str,
    muted_parts: &HashSet<String>,
    soloed_part: Option<&str>,
    theme: &Theme,
) -> Style {
    let mut s = Style::default().add_modifier(Modifier::BOLD);
    if soloed_part == Some(path) {
        s = s.fg(theme.color(Token::Soloed));
    } else if muted_parts.contains(path) {
        s = s.fg(theme.color(Token::MutedFg));
    }
    s
}

/// Whether a column's content should render dimmed.  Muted parts are
/// dim; when a solo is active, everything except the soloed part is
/// dim.
pub fn is_dimmed(
    path: &str,
    muted_parts: &HashSet<String>,
    soloed_part: Option<&str>,
) -> bool {
    if let Some(soloed) = soloed_part {
        soloed != path
    } else {
        muted_parts.contains(path)
    }
}