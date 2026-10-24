//! Mouse hit-testing and dispatch.  Hit-tests return a `HitAction`
//! describing *what* was clicked; `dispatch` is the single place that
//! turns a click into a state mutation.  Views are responsible only for
//! populating `RenderState::tracker_hit` / `vertical_hit` / `spiral`
//! / `notch` each frame — they never mutate anything in response to a
//! mouse event themselves.
//!
//! ## Focus policy
//!
//! Clicking anywhere outside the queue box unfocuses the queue's text
//! input.  This is enforced in `app.rs`'s `handle_mouse`, not here.
//!
//! ## Mute policy
//!
//! Mute and solo are only reachable from headers:
//!
//! * left-click a header → toggle solo
//! * right-click a header → toggle part mute
//! * double-click a header → clear all mutes (handled in `app.rs`)
//!
//! Note-level mute and "clicking a note solos its part" were removed
//! from note dispatch.  Clicking a note now seeks and nothing else.
//!
//! ## Chain marker
//!
//! When a part has an insert chain, its header carries an `@` / `↴`
//! marker at the far right.  Left-click toggles expanded state;
//! right-click toggles disabled.  Marker clicks are intercepted before
//! the header's solo/mute path so clicking `@` never solos the part.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crossterm::event::{MouseButton, MouseEvent};

use crate::mixer::mute;
use crate::player::{Project, Transport};
use crate::tui::state::{NotchTag, RenderState, VerticalRow};

pub enum HitAction {
    /// A spiral cell was clicked.  `measure` is the measure index the
    /// cell represents; if `measures_per_cell > 1` the same measure is
    /// hit for every cell in the bundle.
    SpiralMeasure { measure: usize },
    /// A part label / column header was clicked.  Left = solo toggle,
    /// right = mute toggle.  Clicking a header also unfocuses the
    /// queue's text input — this is the only mouse action that does.
    PartLabel { path: String },
    /// A note body was clicked.  `block_line` and `start_row` identify
    /// the note for seeking.  Left-click seeks to the note's start;
    /// nothing else changes.  Mute and solo are header-only.
    Note { path: String, block_line: usize, start_row: i64 },
    /// A waveform row was clicked.  `fraction` is the horizontal click
    /// position across the view's inner width, already normalised.
    Waveform { path: String, line: usize, fraction: f64 },
    /// A clickable segment of the status notch was clicked.  Covers
    /// both `?` (help) and `⇅` (settings).
    Notch { tag: NotchTag },
    /// The `@` / `↴` chain marker was clicked.  Left toggles expanded,
    /// right toggles disabled (visual-only for now).
    ChainToggle { path: String },
}

pub fn hit_from_spiral(state: &RenderState, m: MouseEvent) -> Option<HitAction> {
    let spiral = state.spiral.as_ref()?;
    let r = spiral.rect;

    if r.width > 0
        && r.height > 0
        && m.column >= r.x
        && m.row >= r.y
        && m.column < r.x + r.width
        && m.row < r.y + r.height
        {
            let lx = (m.column - r.x) as usize;
            let ly = (m.row - r.y) as usize;
            let gw = spiral.layout.grid_w as usize;
            if let Some(measure) = spiral.layout.map.get(ly * gw + lx).copied().flatten() {
                return Some(HitAction::SpiralMeasure { measure });
            }
        }

        let (x0, cols, y) = spiral.layout.tape_span(r.x, r.y)?;
        if m.row != y || m.column < x0 {
            return None;
        }
        let j = (m.column - x0) as usize;
        if j >= cols {
            return None;
        }
        let (measure, _) = spiral.layout.tape_cell_measures(cols, j);
        Some(HitAction::SpiralMeasure { measure })
}

pub fn hit_from_vertical(state: &RenderState, m: MouseEvent) -> Option<HitAction> {
    let hit = state.vertical_hit.as_ref()?;
    let inner = hit.inner;
    if m.column < inner.x
        || m.row < inner.y
        || m.column >= inner.x + inner.width
        || m.row >= inner.y + inner.height
        {
            return None;
        }
        let rel_x = m.column - inner.x;
    let rel_y = m.row - inner.y;
    let row = hit.rows.iter().find(|(y, _)| *y == rel_y)?.1.clone();
    Some(match row {
        VerticalRow::Label { path, marker_x } => {
            let on_marker = marker_x.map_or(false, |(mx0, mx1)| {
                rel_x >= mx0 && rel_x < mx1
            });
            if on_marker {
                HitAction::ChainToggle { path }
            } else {
                HitAction::PartLabel { path }
            }
        }
        VerticalRow::Event { path, block_line, start_row } => {
            HitAction::Note { path, block_line, start_row }
        }
        VerticalRow::Waveform { path, line } => HitAction::Waveform {
            path,
            line,
            fraction: (rel_x as f64) / (inner.width.max(1) as f64),
        },
    })
}

pub fn hit_from_horizontal(state: &RenderState, m: MouseEvent) -> Option<HitAction> {
    let hit = state.tracker_hit.as_ref()?;
    let inner = hit.inner;
    if m.column < inner.x
        || m.row < inner.y
        || m.column >= inner.x + inner.width
        || m.row >= inner.y + inner.height
        {
            return None;
        }
        let rel_x = m.column - inner.x;
    let rel_y = m.row - inner.y;
    for col in &hit.columns {
        if rel_x < col.x_start || rel_x >= col.x_start + col.width {
            continue;
        }
        if rel_y == col.header_y {
            if let Some((mx0, mx1)) = col.marker_x {
                if rel_x >= mx0 && rel_x < mx1 {
                    return Some(HitAction::ChainToggle { path: col.path.clone() });
                }
            }
            return Some(HitAction::PartLabel { path: col.path.clone() });
        }
        let row_idx = (rel_y - col.header_y - 1) as usize;
        let Some((block_line, start_row)) = col.rows.get(row_idx).copied().flatten() else {
            return None;
        };
        return Some(HitAction::Note {
            path: col.path.clone(),
                    block_line,
                    start_row,
        });
    }
    None
}

pub fn hit_from_notch(state: &RenderState, m: MouseEvent) -> Option<HitAction> {
    let p = state.notch.as_ref()?;
    for (tag, r) in &p.rects {
        if m.column >= r.x
            && m.column < r.x + r.width
            && m.row >= r.y
            && m.row < r.y + r.height
            {
                return Some(HitAction::Notch { tag: *tag });
            }
    }
    None
}

/// Seek to a note given its (block_line, start_row).  Shared by the
/// vertical and horizontal note-click paths.
fn seek_to_note(
    tp: &Arc<Transport>,
    p: &Project,
    state: &mut RenderState,
    block_line: usize,
    start_row: i64,
) {
    let measure_samples = p.line_measure_samples(block_line).max(1);
    let rpmeasure = p.rows_per_measure().max(1.0);
    let block_start = p.line_start_sample(block_line);
    let offset = ((start_row as f64) / rpmeasure * (measure_samples as f64)) as i64;
    tp.seek_to.store(block_start + offset, Ordering::Release);
    state.player.last_loop_check_measure = None;
}

pub fn dispatch(
    action: HitAction,
    button: MouseButton,
    tp: &Arc<Transport>,
    p: &Project,
    state: &mut RenderState,
) {
    match action {
        HitAction::SpiralMeasure { measure } => match button {
            MouseButton::Left => {
                let sample = p.line_start_sample(measure);
                tp.seek_to.store(sample, Ordering::Release);
                state.player.last_loop_check_measure = None;
            }
            MouseButton::Right => {
                if !state.player.loop_measures.remove(&measure) {
                    state.player.loop_measures.insert(measure);
                }
                if !state.player.loop_measures.is_empty() {
                    state.player.loop_enabled = true;
                }
                state.player.last_loop_check_measure = None;
            }
            _ => {}
        },
        HitAction::PartLabel { path } => {
            match button {
                MouseButton::Left => mute::toggle_solo(&path),
                MouseButton::Right => mute::toggle_part_mute(&path),
                _ => {}
            }
        }
        HitAction::Note { block_line, start_row, .. } => {
            if button == MouseButton::Left {
                seek_to_note(tp, p, state, block_line, start_row);
            }
        }
        HitAction::Waveform { path: _, line, fraction } => {
            if button != MouseButton::Left {
                return;
            }
            let measure_samples = p.line_measure_samples(line).max(1);
            let block_start = p.line_start_sample(line);
            let offset = (fraction * measure_samples as f64) as i64;
            tp.seek_to.store(block_start + offset, Ordering::Release);
            state.player.last_loop_check_measure = None;
        }
        HitAction::Notch { tag } => {
            if button != MouseButton::Left {
                return;
            }
            match tag {
                NotchTag::Help => {
                    state.player.show_help = !state.player.show_help;
                    if state.player.show_help {
                        state.player.show_config = false;
                    }
                }
                NotchTag::Settings => {
                    if state.player.show_config {
                        state.player.show_config = false;
                    } else {
                        state.config_ui.open(&state.settings);
                        state.player.show_config = true;
                        state.player.show_help = false;
                    }
                }
            }
        }
        HitAction::ChainToggle { path } => match button {
            MouseButton::Left => {
                if !state.expanded_chains.remove(&path) {
                    state.expanded_chains.insert(path);
                }
            }
            MouseButton::Right => {
                // Visual-only for now; audio bypass is a follow-up
                if !state.disabled_chains.remove(&path) {
                    state.disabled_chains.insert(path);
                }
            }
            _ => {}
        },
    }
}
