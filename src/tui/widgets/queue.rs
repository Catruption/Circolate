//! Queue box renderer.  All geometry comes from `crate::tui::layout::QueueLayout`;
//! this file only knows how to draw the box's borders, transport buttons,
//! and buffer lines into the rects it's handed.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
};

use crate::tui::layout::QueueLayout;
use crate::tui::state::{
    Placed, QueueState, GUTTER_W, TEXT_W, VISIBLE_LINES,
};
use crate::tui::theme::{Theme, Token};

pub fn render(
    state: &QueueState,
    layout: &QueueLayout,
    buf: &mut Buffer,
    theme: &Theme,
) -> Placed {
    let set = theme.border_set();
    let edge_style = Style::default()
    .fg(buf.cell((layout.border.right_x, layout.border.top_y))
    .map_or(ratatui::style::Color::Reset, |c| c.fg));

    // Blank the buffer interior so the cover mosaic doesn't bleed
    // through.  The spiral interior is only text-cleared — the cover
    // stays visible behind the spiral cells.
    blank(buf, layout.buffer, theme.color(Token::QueueBg));
    clear_keep_bg(buf, layout.spiral);

    // Border: top edge from left_x to right_x, left wall down to the
    // tape row (frame_bottom_y), corners at top-left and bottom-of-left-wall.
    let b = &layout.border;
    put(buf, b.right_x, b.top_y, set.bottom_right, edge_style);
    for x in b.left_x + 1..b.right_x {
        put(buf, x, b.top_y, set.horizontal_top, edge_style);
    }
    put(buf, b.left_x, b.top_y, set.top_left, edge_style);
    for y in b.top_y + 1..b.frame_bottom_y {
        put(buf, b.left_x, y, set.vertical_left, edge_style);
    }
    put(buf, b.left_x, b.frame_bottom_y, set.bottom_right, edge_style);

    // Transport buttons.
    let on = Style::default()
    .fg(theme.color(Token::QueueTransportActive))
    .add_modifier(ratatui::style::Modifier::BOLD);
    let off = Style::default().fg(theme.color(Token::QueueTransportIdle));
    let label = state.speed_label.as_str();
    put(buf, layout.rewind.x, layout.rewind.y, "◀",
        if state.direction == '◀' { on } else { off });
    put(buf, layout.rewind.x + 2, layout.rewind.y, "▶",
        if state.direction == '▶' { on } else { off });
    put(buf, layout.rewind.x + 3, layout.rewind.y, label,
        if state.direction == '■' { off } else { on });

    // Buffer lines.
    let first = state.cursor.row.saturating_sub(VISIBLE_LINES - 1);
    let text_x = layout.lines.x + GUTTER_W;
    let counter = Style::default().fg(theme.color(Token::QueueCounter));
    let counter_dim = Style::default().fg(theme.color(Token::QueueCounterDim));
    let queue_text = Style::default().fg(theme.color(Token::QueueText));
    let queue_playing = Style::default()
    .fg(theme.color(Token::QueuePlaying))
    .add_modifier(ratatui::style::Modifier::BOLD);
    let cursor = Style::default()
    .fg(theme.color(Token::QueueCursorFg))
    .bg(theme.color(Token::QueueCursorBg));

    for i in 0..VISIBLE_LINES {
        let idx = first + i;
        let y = layout.lines.y + i as u16;
        let Some(line) = state.lines.get(idx) else { continue };

        let st = if line.count > 0 && !line.is_blank() { counter } else { counter_dim };
        put(buf, layout.lines.x, y, &format!("[{}]", line.count), st);

        let text_style = if state.playing == Some(idx) { queue_playing } else { queue_text };
        let show_cursor = state.focused && state.cursor.row == idx;
        for (ci, ch) in line.text.chars().enumerate().take(TEXT_W as usize) {
            let st = if show_cursor && ci == state.cursor.col { cursor } else { text_style };
            put(buf, text_x + ci as u16, y, ch.encode_utf8(&mut [0u8; 4]), st);
        }
        let n = line.text.chars().count();
        if show_cursor && state.cursor.col >= n && state.cursor.col < TEXT_W as usize {
            put(buf, text_x + state.cursor.col as u16, y, " ", cursor);
        }
    }

    Placed {
        box_rect: layout.box_rect,
        spiral: layout.spiral,
        rewind: layout.rewind,
        play: layout.play,
        lines: layout.lines,
        first_line: first,
    }
}

fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) {
    let a = *buf.area();
    if x < a.x || y < a.y || x >= a.right() || y >= a.bottom() {
        return;
    }
    buf.set_stringn(x, y, s, (a.right() - x) as usize, style);
}

fn blank(buf: &mut Buffer, r: Rect, bg: ratatui::style::Color) {
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.reset();
                c.set_bg(bg);
            }
        }
    }
}

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
