//! Modal performance-stats overlay.  Centered box; dims the area behind it.
//! Mirrors `help.rs` in shape — the only difference is the content is
//! driven by the last `PerfSnapshot`, not static text.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::player::perf::PerfSnapshot;
use crate::tui::theme::{Theme, Token};

pub fn draw(f: &mut Frame<'_>, area: Rect, theme: &Theme, perf: &Option<PerfSnapshot>) {
    let w = 62u16.min(area.width.saturating_sub(4));
    let h = 24u16.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h) * 6) / 10;
    let y = y.min(area.y + area.height.saturating_sub(h));
    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);

    let bg = theme.color(Token::HelpBg);
    let head = Style::default().fg(theme.color(Token::HelpKey)).bg(bg)
        .add_modifier(Modifier::BOLD);
    let key = Style::default().fg(theme.color(Token::HelpKey)).bg(bg);
    let fg  = Style::default().fg(theme.color(Token::HelpFg)).bg(bg);
    let dim = Style::default().fg(theme.color(Token::HelpDim)).bg(bg);

    let row = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("  {:<10}", k), key),
            Span::styled(v, fg),
        ])
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled("", fg)));

    match perf {
        None => {
            lines.push(Line::from(Span::styled(
                "  collecting the first 1s of samples…", dim)));
        }
        Some(p) => {
            // ── Audio ──
            lines.push(Line::from(Span::styled("  Audio render (last 1s)", head)));
            lines.push(row("load",
                format!("{:>6.1}%   worst {:>6.1}%   overruns {}",
                    p.load * 100.0, p.worst * 100.0, p.overruns)));
            lines.push(row("stages",
                format!("voices {:>4.1}%  vst {:>4.1}%  shim {:>4.1}%  fx {:>4.1}%",
                    p.voices * 100.0, p.vst * 100.0, p.shim * 100.0, p.fx * 100.0)));

            // ── UI ──
            lines.push(Line::from(Span::styled("", fg)));
            lines.push(Line::from(Span::styled("  UI frame (last 1s)", head)));
            lines.push(row("frames",
                format!("{:>4}   avg {:>6.2}ms   worst {:>6.2}ms",
                    p.ui_frames, p.ui_avg_ms, p.ui_worst_ms)));

            // ── Cache ──
            lines.push(Line::from(Span::styled("", fg)));
            lines.push(Line::from(Span::styled("  Cache (delta since last poll)", head)));
            let total = p.cache_hits + p.cache_misses;
            let rate = if total > 0 { p.cache_hits as f64 / total as f64 * 100.0 } else { 0.0 };
            lines.push(row("parts",
                format!("hits {:>5}   misses {:>3}   rate {:>5.1}%",
                    p.cache_hits, p.cache_misses, rate)));

            // ── Worker ──
            lines.push(Line::from(Span::styled("", fg)));
            lines.push(Line::from(Span::styled("  Worker (loader thread)", head)));
            lines.push(row("load_due",
                format!("{} call(s)   avg {:>6.2}ms",
                    p.worker_loads, p.worker_avg_ms)));

            // ── Analysis ──
            lines.push(Line::from(Span::styled("", fg)));
            lines.push(Line::from(Span::styled(
                "  Analysis (wall-clock this second)", head)));
            lines.push(row("report",
                format!("{:>7.2}ms   {:>5.1}% of wall",
                    p.analysis_ms, p.analysis_ms / 10.0)));
            lines.push(row("motion",
                format!("{:>7.2}ms   {:>5.1}% of wall",
                    p.motion_ms, p.motion_ms / 10.0)));
        }
    }

    lines.push(Line::from(Span::styled("", fg)));
    lines.push(Line::from(Span::styled("  press P to close", dim)));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set())
        .border_style(Style::default().fg(theme.color(Token::HelpKey)))
        .title(" Performance ")
        .style(Style::default().bg(bg));

    f.render_widget(Paragraph::new(lines).block(block), rect);
}