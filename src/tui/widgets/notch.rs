//! Status notch: a bracketed strip on the tracker panel's bottom-left
//! corner, replacing the `╚` with `[seg seg ...]` and leaving the rest
//! of the bottom border as `═`.  Segments can be clickable (help) or
//! plain status text (blocklist position, tempo, …).
//!
//! Call this *after* the tracker view has drawn its outer block and
//! *after* the queue box has claimed the bottom-right, so the notch
//! knows how much room it has.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use crate::tui::state::{NotchPlaced, NotchTag};
use crate::tui::theme::{Theme, Token};

/// One rendered segment.  `tag` is `Some` if clicking it should do
/// something; `emphasized` picks the accent colour over the plain
/// border colour.  `color` overrides `emphasized` when set, so a
/// segment can have its own accent (e.g. Settings in cyan).
pub struct Segment {
    pub text: String,
    pub tag: Option<NotchTag>,
    pub emphasized: bool,
    pub color: Option<Color>,
}

pub fn render(
    segments: &[Segment],
    tracker: Rect,
    max_x: u16,
    buf: &mut Buffer,
    theme: &Theme,
) -> Option<NotchPlaced> {
    if tracker.width < 4 || tracker.height < 3 {
        return None;
    }

    let content_w: usize = segments
    .iter()
    .map(|s| s.text.chars().count())
    .sum::<usize>()
    + segments.len().saturating_sub(1);
    let notch_w = content_w + 2;

    let x0 = tracker.x;
    let x1 = x0.saturating_add(notch_w as u16);
    if x1 > max_x {
        return None;
    }
    let y = tracker.bottom() - 1;

    let border_style = Style::default().fg(theme.color(Token::Border));
    let emphasis = Style::default()
    .fg(theme.color(Token::HelpButton))
    .add_modifier(Modifier::BOLD);

    put(buf, x0, y, "[", border_style);
    let mut cursor = x0 + 1;
    let mut rects: Vec<(NotchTag, Rect)> = Vec::new();

    for (i, seg) in segments.iter().enumerate() {
        if i > 0 {
            put(buf, cursor, y, " ", border_style);
            cursor += 1;
        }
        let style = if let Some(c) = seg.color {
            Style::default().fg(c).add_modifier(Modifier::BOLD)
        } else if seg.emphasized {
            emphasis
        } else {
            border_style
        };
        put(buf, cursor, y, &seg.text, style);
        let w = seg.text.chars().count() as u16;
        if let Some(tag) = seg.tag {
            rects.push((tag, Rect::new(cursor, y, w.max(1), 1)));
        }
        cursor += w;
    }
    put(buf, cursor, y, "]", border_style);

    Some(NotchPlaced { rects })
}

fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) {
    let a = *buf.area();
    if x < a.x || y < a.y || x >= a.right() || y >= a.bottom() {
        return;
    }
    buf.set_stringn(x, y, s, (a.right() - x) as usize, style);
}
