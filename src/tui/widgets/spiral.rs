//! Spiral playbar renderer.  The coil is drawn into `area` (the rect the
//! queue box reserved); the straight tape continues from the coil's bottom-left
//! corner along the same row, to the left, out to `tape_left`.  Everything is
//! double-line.  Records where it drew for hit-testing.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use std::sync::atomic::Ordering;

use crate::player::{Project, Transport};
use crate::tui::spiral::SpiralLayout;
use crate::tui::state::{RenderState, SpiralState};
use crate::tui::theme::{PathDirs, Theme, Token};

const PLAYHEAD_LOOKAHEAD_SECS: f64 = 0.05;

/// Mark the direction from (gx, gy) toward a path neighbour (dominant axis).
fn link_toward(dirs: &mut PathDirs, gx: usize, gy: usize, nx: usize, ny: usize) {
    let dx = nx as i32 - gx as i32;
    let dy = ny as i32 - gy as i32;
    if dx.abs() >= dy.abs() {
        if dx < 0 { dirs.left = true; } else if dx > 0 { dirs.right = true; }
    } else if dy < 0 {
        dirs.up = true;
    } else {
        dirs.down = true;
    }
}

/// Double-line glyph for a cell with the given connections.
fn coil_glyph(d: &PathDirs) -> &'static str {
    match (d.left, d.right, d.up, d.down) {
        (true, true, false, false) => "═",
        (false, false, true, true) => "║",
        (true, false, false, true) => "╗",
        (false, true, false, true) => "╔",
        (false, true, true, false) => "╚",
        (true, false, true, false) => "╝",
        (true, true, false, true) => "╦",
        (true, true, true, false) => "╩",
        (false, true, true, true) => "╠",
        (true, false, true, true) => "╣",
        (true, true, true, true) => "╬",
        (true, false, false, false) | (false, true, false, false) => "═",
        (false, false, true, false) | (false, false, false, true) => "║",
        (false, false, false, false) => "•",
    }
}

fn spiral_dirs(layout: &SpiralLayout, gx: usize, gy: usize) -> PathDirs {
    let gw = layout.grid_w as usize;
    let Some(m) = layout.map.get(gy * gw + gx).copied().flatten() else {
        return PathDirs::default();
    };

    // Our index in the coil path (outer end = 0, centre = last).
    let idx = m.saturating_sub(layout.coil_start);
    let mut dirs = PathDirs::default();

    if idx > 0 {
        let (px, py) = layout.path[idx - 1];
        link_toward(&mut dirs, gx, gy, px as usize, py as usize);
    } else if layout.coil_start > 0 {
        // First coil cell: the tape arrives from the left.
        dirs.left = true;
    }

    if idx + 1 < layout.path.len() {
        let (nx, ny) = layout.path[idx + 1];
        link_toward(&mut dirs, gx, gy, nx as usize, ny as usize);
    }

    // Close the curl at the centre (end of the song): keep turning the way
    // the spiral has been turning (counter-clockwise inward).
    if layout.is_center(gx, gy) {
        let (dx, dy) = if idx > 0 {
            let (px, py) = layout.path[idx - 1];
            (gx as i32 - px as i32, gy as i32 - py as i32)
        } else if layout.coil_start > 0 {
            (1, 0) // arrived from the tape, moving right
        } else {
            (0, 0)
        };
        if dx > 0 {
            dirs.up = true;
        } else if dy < 0 {
            dirs.left = true;
        } else if dy > 0 {
            dirs.right = true;
        } else {
            dirs.down = true; // moving left, or a lone cell
        }
    }

    dirs
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    f: &mut Frame<'_>,
    area: Rect,
    tape_left: u16,
    tp: &Transport,
    p: &Project,
    state: &mut RenderState,
    theme: &Theme,
    mut layout: SpiralLayout,
) {
    layout.tape_left = tape_left;
    state.spiral = Some(SpiralState { layout: layout.clone(), rect: area });

    let lookahead = (PLAYHEAD_LOOKAHEAD_SECS * p.sample_rate as f64) as i64;
    let pos = (tp.position.load(Ordering::Relaxed) + lookahead).min(p.total_samples);
    let current = p.current_line(pos);

    let current_fg = theme.color(Token::SpiralCurrent);
    let played_fg = theme.color(Token::SpiralPlayed);
    let future_fg = theme.color(Token::SpiralFuture);
    let loop_bg = theme.color(Token::SpiralLoopBg);

    // Style for a cell covering measures m .. m + len.
    let style_for = |m: usize, len: usize| -> Style {
        let mut style = match current {
            Some(cur) if cur >= m && cur < m + len => Style::default()
            .fg(current_fg)
            .add_modifier(Modifier::BOLD),
            Some(cur) if cur >= m + len => Style::default().fg(played_fg),
            _ => Style::default().fg(future_fg),
        };
        if (m..m + len).any(|mm| state.player.loop_measures.contains(&mm)) {
            style = style.bg(loop_bg);
        }
        style
    };

    // The coil.
    let gw = layout.grid_w as usize;
    let gh = layout.grid_h as usize;
    let mpcell = layout.measures_per_cell.max(1);
    let mut lines: Vec<Line> = Vec::with_capacity(gh);
    for gy in 0..gh {
        let mut spans: Vec<Span> = Vec::with_capacity(gw);
        for gx in 0..gw {
            match layout.map.get(gy * gw + gx).copied().flatten() {
                None => spans.push(Span::raw(" ")),
                Some(m) => {
                    let dirs = spiral_dirs(&layout, gx, gy);
                    spans.push(Span::styled(coil_glyph(&dirs), style_for(m, mpcell)));
                }
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);

    // The tape: measures 0..coil_start, along the coil's bottom row from
    // `tape_left` into the coil's corner.  Few measures are stretched over the
    // columns, many are grouped.  Drawn last so it also covers the blank cells
    // of the coil's own bottom row.
    if let Some((x0, cols, y)) = layout.tape_span(area.x, area.y) {
        let buf = f.buffer_mut();
        for j in 0..cols {
            let (m, len) = layout.tape_cell_measures(cols, j);
            if let Some(cell) = buf.cell_mut((x0 + j as u16, y)) {
                cell.set_symbol("═");
                cell.set_style(style_for(m, len));
            }
        }
    }
}
