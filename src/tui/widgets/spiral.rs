//! Spiral playbar: one cell per measure, walking outward from the
//! centre.  The path geometry lives in `tui::spiral`; this module
//! renders a laid-out spiral and records where it drew for hit-testing.

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
use crate::tui::theme::{path_glyph, PathDirs, Theme, Token};

const PLAYHEAD_LOOKAHEAD_SECS: f64 = 0.05;

/// Which grid neighbours of `(gx, gy)` are on the spiral path at the
/// adjacent step.  Local because it's only meaningful in terms of
/// `SpiralLayout`'s internals and has no other consumer.
fn spiral_dirs(layout: &SpiralLayout, gx: usize, gy: usize) -> PathDirs {
    let gw = layout.grid_w as usize;
    let gh = layout.grid_h as usize;
    let Some(m) = layout.map.get(gy * gw + gx).copied().flatten() else {
        return PathDirs::default();
    };
    let step = layout.measures_per_cell as i64;
    let m = m as i64;
    let check = |nx: usize, ny: usize| -> bool {
        layout
        .map
        .get(ny * gw + nx)
        .copied()
        .flatten()
        .map(|n| (n as i64 - m).abs() == step)
        .unwrap_or(false)
    };
    let mut dirs = PathDirs {
        left: gx > 0 && check(gx - 1, gy),
        right: gx + 1 < gw && check(gx + 1, gy),
        up: gy > 0 && check(gx, gy - 1),
        down: gy + 1 < gh && check(gx, gy + 1),
    };

    // The centre cell has no "down" neighbour by construction (the
    // path leaves it going right), but visually the curl reads better
    // as a corner.  Fake the edge; the glyph picker turns the pair
    // into `╔`.
    if layout.is_center(gx, gy) && !dirs.down {
        dirs.down = true;
    }

    dirs
}

pub fn draw(
    f: &mut Frame<'_>,
    area: Rect,
    tp: &Transport,
    p: &Project,
    state: &mut RenderState,
    theme: &Theme,
    layout: SpiralLayout,
) {
    state.spiral = Some(SpiralState { layout: layout.clone(), rect: area });

    let lookahead = (PLAYHEAD_LOOKAHEAD_SECS * p.sample_rate as f64) as i64;
    let pos = (tp.position.load(Ordering::Relaxed) + lookahead).min(p.total_samples);
    let current = p.current_line(pos);

    let gw = layout.grid_w as usize;
    let gh = layout.grid_h as usize;
    let mpcell = layout.measures_per_cell;

    let current_fg = theme.color(Token::SpiralCurrent);
    let played_fg = theme.color(Token::SpiralPlayed);
    let future_fg = theme.color(Token::SpiralFuture);
    let loop_bg = theme.color(Token::SpiralLoopBg);

    let mut lines: Vec<Line> = Vec::with_capacity(gh);
    for gy in 0..gh {
        let mut spans: Vec<Span> = Vec::with_capacity(gw);
        for gx in 0..gw {
            let idx = gy * gw + gx;
            let measure = layout.map.get(idx).copied().flatten();
            let (glyph, mut style) = match measure {
                None => (" ", Style::default()),
                Some(m) => {
                    let dirs = spiral_dirs(&layout, gx, gy);
                    let g = path_glyph(dirs, theme.border);
                    let style = match current {
                        Some(cur) if cur >= m && cur < m + mpcell => Style::default()
                        .fg(current_fg)
                        .add_modifier(Modifier::BOLD),
                        Some(cur) if cur >= m + mpcell => Style::default().fg(played_fg),
                        _ => Style::default().fg(future_fg),
                    };
                    (g, style)
                }
            };

            if let Some(m) = measure {
                let in_loop = (m..m + mpcell)
                .any(|mm| state.player.loop_measures.contains(&mm));
                if in_loop {
                    style = style.bg(loop_bg);
                }
            }

            spans.push(Span::styled(glyph.to_string(), style));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
}
