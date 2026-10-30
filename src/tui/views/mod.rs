//! View selection and the frame-level draw entry point.

pub mod horizontal;
pub mod measure;
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
use crate::tui::layout::{self, QueueLayout};
use crate::tui::spiral::{build_coil, coil_sizes, SpiralLayout};
use crate::tui::state::{RenderState, SpiralCacheKey, SpiralCache, BUF_W};
use crate::tui::theme::Theme;
use crate::tui::widgets;
use crate::tui::widgets::cover::Cover;

const RELOAD_FLASH_MS: u64 = 1500;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View { Horizontal, Vertical }

pub fn select(area: Rect, forced: Option<View>) -> View {
    if let Some(v) = forced { return v; }
    if area.height as f32 * 2.0 > area.width as f32 { View::Vertical } else { View::Horizontal }
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
    if let Some(msg) = &state.loading {
        let chunks = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([ratatui::layout::Constraint::Length(3), ratatui::layout::Constraint::Min(0)])
        .split(f.area());
        let header = ratatui::widgets::Paragraph::new(msg.as_str())
        .block(state.settings.theme.block().title(" Loading "));
        f.render_widget(header, chunks[0]);
        crate::tui::widgets::logs::draw(f, chunks[1], state);
        return;
    }
    let area = f.area();

    let rc = tp.reload_count.load(Ordering::Relaxed);
    if rc > state.player.last_reload_count {
        state.player.last_reload_count = rc;
        state.player.reload_flash_until = Some(Instant::now() + Duration::from_millis(RELOAD_FLASH_MS));
    }

    refresh_cover(state, project, area);

    let theme: Theme = state.settings.theme.clone();
    let forced = forced_from_config(state.settings.view_mode.as_deref());

    let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([Constraint::Length(3), Constraint::Min(6)])
    .split(area);

    draw_header(f, chunks[0], project, tp, sample_rate, state.loading.as_deref(), &theme);

    // Refresh motion highlights so both views can render red-text notes.
    let pos_now = tp.position.load(Ordering::Relaxed).max(0);
    if let Some(li_now) = project.current_line(pos_now) {
        let report = crate::player::analysis::analyze(project, tp, li_now);
        state.motion_highlights = report.motion_highlight.clone();
        if state.player.show_analysis {
            // We already have the report — draw it last, on top of everything.
            // (handled below after the views render)
        }
    } else {
        state.motion_highlights.clear();
    }

    let tape_rows: u16 = if chunks[1].height > 6 { 1 } else { 0 };
    let tracker_area = Rect { height: chunks[1].height - tape_rows, ..chunks[1] };

    match select(area, forced) {
        View::Horizontal => {
            state.vertical_hit = None;
            horizontal::draw(f, tracker_area, tp, project, state);
        }
        View::Vertical => {
            state.tracker_hit = None;
            vertical::draw(f, tracker_area, tp, project, state);
        }
    }

    widgets::cover::paint_background(f.buffer_mut(), state.cover.as_ref());
    widgets::lyrics::draw(f, chunks[1], tp, project);

    let spd = tp.speed.load(Ordering::Relaxed);
    state.queue.direction = match spd.signum() { 1 => '▶', -1 => '◀', _ => '■' };
    state.queue.speed_label = format!("{}x", spd.abs().max(1));

    let total_measures = project.line_count();
    let aspect = state.settings.layout.spiral_aspect_x.max(1);
    let max_h_cfg = state.settings.layout.spiral_max_h.max(2);

    let tape_left = chunks[1].left();
    let budget_w = (chunks[1].width as usize / 2).max(8);
    let budget_h = max_h_cfg.min((chunks[1].height as usize).saturating_sub(2).max(2));
    let speed_label_w = state.queue.speed_label.chars().count() as u16;

    let cache_key = SpiralCacheKey {
        total_measures, aspect, budget_w, budget_h,
        tape_left, area: chunks[1], tape_rows, speed_label_w,
    };

    let best = match &state.spiral_cache {
        Some(c) if c.key == cache_key => c.result.clone(),
        _ => {
            let mut best: Option<(SpiralLayout, QueueLayout)> = None;
            for n in coil_sizes(total_measures, aspect) {
                let mut layout = build_coil(total_measures, n, aspect);
                layout.tape_left = tape_left;
                if layout.grid_w as usize > budget_w || layout.grid_h as usize > budget_h { break; }
                let size = (layout.grid_w.max(1), layout.grid_h);
                let Some(queue_layout) = layout::compute_queue(
                    chunks[1], tape_rows, size, BUF_W, speed_label_w,
                ) else {
                    if best.is_some() { break; }
                    continue;
                };
                let fits = layout.tape_fits(queue_layout.spiral.x);
                best = Some((layout, queue_layout));
                if fits { break; }
            }
            state.spiral_cache = Some(SpiralCache { key: cache_key, result: best.clone() });
            best
        }
    };

    let (spiral_layout, queue_placed) = match best {
        Some((spiral_layout, queue_layout)) => {
            let placed = widgets::queue::render(&state.queue, &queue_layout, f.buffer_mut(), &theme);
            (Some(spiral_layout), Some(placed))
        }
        None => (None, None),
    };
    state.queue_placed = queue_placed;

    let notch_max_x = queue_placed.map_or(chunks[1].right().saturating_sub(1), |q| q.box_rect.x);
    let segments = build_notch_segments(state);
    state.notch = widgets::notch::render(&segments, tracker_area, notch_max_x, f.buffer_mut(), &theme);

    match (queue_placed, spiral_layout) {
        (Some(p), Some(layout)) => widgets::spiral::draw(f, p.spiral, tape_left, tp, project, state, &theme, layout),
        _ => state.spiral = None,
    }

    if state.player.show_help {
        widgets::help::draw(f, area, &theme);
    }

    if state.player.show_config {
        widgets::config::draw(f, area, &state.config_ui, &state.settings);
    }

    if state.player.show_analysis {
        let pos = tp.position.load(Ordering::Relaxed).max(0);
        if let Some(li) = project.current_line(pos) {
            let report = crate::player::analysis::analyze(project, tp, li);
            widgets::analysis::draw(f, area, &theme, li, &report);
        }
    }
}

fn draw_header(
    f: &mut Frame<'_>, area: Rect, p: &Project, tp: &Transport,
    sr: u32, loading: Option<&str>, theme: &Theme,
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
                              p.initial_bpm.round() as i64, num, den, p.voice_count,
                                            p.line_count(), speed_tag, secs, total
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

fn build_notch_segments(state: &RenderState) -> Vec<widgets::notch::Segment> {
    use crate::tui::state::NotchTag;
    use crate::tui::widgets::notch::Segment;
    vec![
        Segment { text: state.settings.layout.help_glyph.clone(), tag: Some(NotchTag::Help), emphasized: true, color: None },
        Segment { text: "⇅".into(), tag: Some(NotchTag::Settings), emphasized: false, color: Some(Color::Cyan) },
        Segment { text: "Σ".into(), tag: Some(NotchTag::Analysis), emphasized: false, color: Some(Color::Magenta) },
    ]
}
