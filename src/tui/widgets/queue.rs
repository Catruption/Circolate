//! Queue box: a notch in the tracker's bottom-right corner.  The buffer
//! and transport sit anchored to the tracker's right edge; the spiral
//! playbar is to their left and grows/shrinks to the left as the song's
//! measure count changes.  That way reloading a song never moves the
//! queue.
//!
//! Visual (bottom-right of the tracker panel):
//!
//! ```text
//!                      ┌───────────┐
//!                      │  ◀ ▶ 1x   │
//!    [spiral grid]     │[1]song…   │
//!                      │[2]song…   │
//!                      │[0]song…   │
//!                      │[1]song…   │
//!                      │[9]song…   │
//!                      └───────────┘
//! ```

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
};

use crate::tui::state::{
    Placed, QueueState, BUF_W, GUTTER_W, NOTCH_H, TEXT_W, VISIBLE_LINES,
};
use crate::tui::theme::{Theme, Token};

pub fn render(
    state: &QueueState,
    tracker: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    spiral_size: (u16, u16),
) -> Option<Placed> {
    let sw = spiral_size.0.max(1);
    let sh = spiral_size.1.max(1).min(NOTCH_H - 1);
    // Notch = 1 (left border) + spiral + buffer.
    let nw = 1 + sw + BUF_W;
    if tracker.width < nw + 2 || tracker.height < NOTCH_H + 3 {
        return None;
    }
    let set = theme.border_set();
    let edge = Style::default()
        .fg(buf.cell((tracker.x, tracker.y)).map_or(ratatui::style::Color::Reset, |c| c.fg));

    let right = tracker.right() - 1;
    let bottom = tracker.bottom() - 1;
    let nx = right + 1 - nw;
    let ny = tracker.bottom() - NOTCH_H;
    let sx = nx + 1;       // spiral starts here
    let bx = sx + sw;      // buffer starts here

    // The buffer side gets a solid background so text stays legible over
    // the cover mosaic.  The spiral side is only cleared of tracker
    // text; its background is left alone so the cover shows through.
    blank(
        buf,
        Rect::new(bx, ny + 1, BUF_W, NOTCH_H - 1),
        theme.color(Token::QueueBg),
    );
    clear_keep_bg(buf, Rect::new(sx, ny + 1, sw, NOTCH_H - 1));

    // Tracker frame, rerouted around the notch.
    put(buf, right, ny, set.bottom_right, edge);
    for x in nx + 1..right {
        put(buf, x, ny, set.horizontal_top, edge);
    }
    put(buf, nx, ny, set.top_left, edge);
    for y in ny + 1..bottom {
        put(buf, nx, y, set.vertical_left, edge);
    }
    put(buf, nx, bottom, set.bottom_right, edge);

    // Transport buttons: right-aligned inside the buffer's horizontal
    // span, at the row just below the notch's top edge.
    let label = state.speed_label.as_str();
    let lw = label.chars().count() as u16;
    let tx = right - 3 - lw;
    let ty = ny + 1;
    let on = Style::default()
        .fg(theme.color(Token::QueueTransportActive))
        .add_modifier(ratatui::style::Modifier::BOLD);
    let off = Style::default().fg(theme.color(Token::QueueTransportIdle));
    put(buf, tx, ty, "◀", if state.direction == '◀' { on } else { off });
    put(buf, tx + 2, ty, "▶", if state.direction == '▶' { on } else { off });
    put(buf, tx + 3, ty, label, if state.direction == '■' { off } else { on });

    // Buffer lines: `[n]` gutter, then the song name.
    let first = state.cursor.row.saturating_sub(VISIBLE_LINES - 1);
    let lines_y = ny + 2;
    let text_x = bx + GUTTER_W;
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
        let y = lines_y + i as u16;
        let Some(line) = state.lines.get(idx) else { continue };

        let st = if line.count > 0 && !line.is_blank() { counter } else { counter_dim };
        put(buf, bx, y, &format!("[{}]", line.count), st);

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

    Some(Placed {
        notch: Rect::new(nx, ny, nw, NOTCH_H),
        // Spiral is bottom-aligned within the notch's left side.
        spiral: Rect::new(sx, bottom + 1 - sh, sw, sh),
        rewind: Rect::new(tx, ty, 2, 1),
        play: Rect::new(tx + 2, ty, 1 + lw, 1),
        lines: Rect::new(bx, lines_y, BUF_W, VISIBLE_LINES as u16),
        first_line: first,
    })
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