//! Mixer overlay for adjusting global mix parameters.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use crate::player::Transport;
use crate::tui::theme::{Theme, Token};

pub fn draw(f: &mut Frame<'_>, area: Rect, theme: &Theme, tp: &Transport, accent: Color) {
    let w = 40u16.min(area.width.saturating_sub(4));
    let h = 10u16.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);

    let width = f32::from_bits(tp.stereo_width.load(std::sync::atomic::Ordering::Relaxed));
    let bar_len = ((width / 3.0) * 20.0).clamp(0.0, 20.0) as usize;
    let bar = format!("[{}{}] {:.2}x", "█".repeat(bar_len), "░".repeat(20 - bar_len), width);

    let key = Style::default().fg(accent).add_modifier(Modifier::BOLD);
    let val = Style::default().fg(theme.color(Token::QueueCounter));

    let lines = vec![
        Line::from(Span::styled("", Style::default())),
        Line::from(vec![
            Span::styled("  Stereo Width: ", key),
                   Span::styled(bar, val),
        ]),
        Line::from(Span::styled("", Style::default())),
        Line::from(Span::styled("  ←/→  Adjust Width (0.0 - 3.0)", Style::default().fg(theme.color(Token::HelpDim)))),
        Line::from(Span::styled("  Esc  Close", Style::default().fg(theme.color(Token::HelpDim)))),
    ];

    let block = Block::default()
    .borders(Borders::ALL)
    .border_set(theme.border_set())
    .border_style(Style::default().fg(accent))
    .title(" Mixer ")
    .style(Style::default().bg(theme.color(Token::HelpBg)));

    f.render_widget(Paragraph::new(lines).block(block), rect);
}
