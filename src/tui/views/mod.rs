//! View selection and the frame-level draw entry point.  Two views
//! share the header and the queue box but render the tracker area
//! completely differently — they're picked by terminal aspect ratio,
//! or forced by config.

pub mod horizontal;
pub mod vertical;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::player::{Project, Transport};
use crate::tui::spiral::{build_spiral_with_aspect, SpiralLayout};
use crate::tui::state::{RenderState, VISIBLE_LINES};
use crate::tui::theme::Theme;
use crate::tui::widgets;
use crate::tui::widgets::cover::Cover;

const RELOAD_FLASH_MS: u64 = 1500;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View { Horizontal, Vertical }

pub fn select(area: Rect, forced: Option<View>) -> View {
    if let Some(v) = forced { return v; }
    if area.height as f32 * 2.0 > area.width as f32 {
        View::Vertical
    } else {
        View::Horizontal
    }
}

pub fn forced_from_config(mode: Option<&str>) -> Option<View> {
    match mode {
        Some("horizontal") => Some(View::Horizontal),
        Some("vertical") => Some(View::Vertical),
        _ => None,
    }
}

pub fn draw(
    f: &mut Frame<'_>,
    tp: &Transport,
    project: &Project,
    sample_rate: u32,
    state: &mut RenderState,
) {
    let area = f.area();

    let rc = tp.reload_count.load(Ordering::Relaxed);
    if rc > state.player.last_reload_count {
        state.player.last_reload_count = rc;
        state.player.reload_flash_until =
            Some(Instant::now() + Duration::from_millis(RELOAD_FLASH_MS));
    }

    refresh_cover(state, project, area);

    let theme: Theme = state.settings.theme.clone();
    let forced = forced_from_config(state.settings.view_mode.as_deref());

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(6)])
        .split(area);

    draw_header(f, chunks[0], project, tp, sample_rate, state.loading.as_deref(), &theme);

    match select(area, forced) {
        View::Horizontal => {
            state.vertical_hit = None;
            horizontal::draw(f, chunks[1], tp, project, state);
        }
        View::Vertical => {
            state.tracker_hit = None;
            vertical::draw(f, chunks[1], tp, project, state);
        }
    }

    widgets::cover::paint_background(f.buffer_mut(), state.cover.as_ref());

    let spd = tp.speed.load(Ordering::Relaxed);
    state.queue.direction = match spd.signum() {
        1 => '▶', -1 => '◀', _ => '■',
    };
    state.queue.speed_label = format!("{}x", spd.abs().max(1));

    let total_measures = project.line_count();
    let aspect = state.settings.layout.spiral_aspect_x.max(1);
    let max_h = state.settings.layout.spiral_max_h.min(VISIBLE_LINES).max(2);
    let spiral_layout: SpiralLayout =
        build_spiral_with_aspect(total_measures, max_h, aspect);
    let spiral_size = (spiral_layout.grid_w.max(1), spiral_layout.grid_h);

    // Queue box first: it claims the bottom-right.
    let queue_placed = widgets::queue::render(
        &state.queue,
        chunks[1],
        f.buffer_mut(),
        &theme,
        spiral_size,
    );
    state.queue_placed = queue_placed;

    // Notch second: it claims the bottom-left, stopping short of the
    // queue box.  Left as `None` if the segments don't fit before the
    // queue, in which case the outer `╚` stays visible.
    let notch_max_x = queue_placed
        .map_or(chunks[1].right().saturating_sub(1), |q| q.notch.x);
    let segments = build_notch_segments(state);
    state.notch = widgets::notch::render(
        &segments,
        chunks[1],
        notch_max_x,
        f.buffer_mut(),
        &theme,
    );

    match queue_placed {
        Some(p) => widgets::spiral::draw(
            f,
            p.spiral,
            tp,
            project,
            state,
            &theme,
            spiral_layout,
        ),
        None => state.spiral = None,
    }

    if state.player.show_help {
        widgets::help::draw(f, area, &theme);
    }
}

fn draw_header(
    f: &mut Frame<'_>,
    area: Rect,
    p: &Project,
    tp: &Transport,
    sr: u32,
    loading: Option<&str>,
    theme: &Theme,
) {
    let (num, den) = p.time_signature;
    let pos = tp.position.load(Ordering::Relaxed).max(0) as f64;
    let secs = pos / sr as f64;
    let total = p.total_secs();
    let speed = tp.speed.load(Ordering::Relaxed);
    let speed_tag = match speed {
        0 => "■".to_string(),
        s if s > 0 => format!("▶{}x", s),
        s => format!("◀{}x", -s),
    };
    let title = loading.unwrap_or(&p.title);
    let line = Line::from(vec![
        Span::styled(" 🎵 ", Style::default().fg(Color::Cyan)),
        Span::styled(title.to_string(), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            "    {} BPM    {}/{}    {} voices    {} lines    {}  {:>5.1}s / {:.1}s",
            p.initial_bpm.round() as i64,
            num, den, p.voice_count, p.line_count(),
            speed_tag, secs, total
        )),
    ]);
    f.render_widget(Paragraph::new(line).block(theme.block()), area);
}

fn refresh_cover(state: &mut RenderState, project: &Project, area: Rect) {
    let dims = (area.width as usize, area.height as usize);
    if state.cover_source.as_deref() != project.cover_path() || state.cover_dims != dims {
        let dim = state.settings.layout.cover_dim;
        state.cover = project.cover_path()
            .and_then(|path| Cover::load(path, dims.0, dims.1, dim));
        state.cover_source = project.cover_path().map(Path::to_path_buf);
        state.cover_dims = dims;
    }
}

/// The notch's current contents.  Add segments here — for example a
/// `blocklist N/M` status cell — and they'll render automatically.
fn build_notch_segments(state: &RenderState) -> Vec<widgets::notch::Segment> {
    use crate::tui::state::NotchTag;
    use crate::tui::widgets::notch::Segment;

    vec![Segment {
        text: state.settings.layout.help_glyph.clone(),
        tag: Some(NotchTag::Help),
        emphasized: true,
    }]
}