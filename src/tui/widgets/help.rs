//! Modal help overlay.  Centered box; dims the area behind it.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::tui::theme::{Theme, Token};

pub fn draw(f: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let w = 56u16.min(area.width.saturating_sub(4));
    let h = 22u16.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;

    // FIXED: Biased vertical centering (60% top, 40% bottom)
    let y = area.y + (area.height.saturating_sub(h) * 6) / 10;
    let y = y.min(area.y + area.height.saturating_sub(h));

    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);

    let help_bg = theme.color(Token::HelpBg);
    let key = Style::default()
    .fg(theme.color(Token::HelpKey))
    .bg(help_bg)
    .add_modifier(Modifier::BOLD);
    let desc = Style::default().fg(theme.color(Token::HelpFg)).bg(help_bg);
    let dim = Style::default().fg(theme.color(Token::HelpDim)).bg(help_bg);

    let row = |k: &str, d: &str| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("  {:<14}", k), key),
                   Span::styled(d.to_string(), desc),
        ])
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled("", desc)));
    lines.push(row("← →", "play reverse / forward"));
    lines.push(row("hold ← →", "accelerate through 4x, 16x"));
    lines.push(row("home", "rewind to start"));
    lines.push(row("[ ]", "prev / next block"));
    lines.push(row("h l", "prev / next column focus"));
    lines.push(row("m / s", "mute / solo focused column"));
    lines.push(row("c", "clear all mutes and solos"));
    lines.push(row("L", "toggle loop playback"));
    lines.push(row("v", "toggle mute mode (clicks = right-click)"));
    lines.push(row("click queue", "type · enter plays · esc leaves"));
    lines.push(row("pgup / pgdn", "loop count (or click [n])"));
    lines.push(row("?", "toggle this help"));
    lines.push(row("a", "toggle analysis"));
    lines.push(row("q / esc", "quit"));
    lines.push(Line::from(Span::styled("", desc)));
    lines.push(Line::from(Span::styled("  Mouse:", key)));
    lines.push(Line::from(Span::styled(
        "  Left-click    spiral: seek   note: jump   header: focus",
        dim,
    )));
    lines.push(Line::from(Span::styled(
        "  Right-click   spiral: loop   note: mute   header: mute / solo",
        dim,
    )));
    lines.push(Line::from(Span::styled(
        "  Double-click  clear all mutes and solos",
        dim,
    )));

    let block = Block::default()
    .borders(Borders::ALL)
    .border_set(theme.border_set())
    .border_style(Style::default().fg(theme.color(Token::HelpKey)))
    .title(" Help ")
    .style(Style::default().bg(help_bg));

    f.render_widget(Paragraph::new(lines).block(block), rect);
}
