use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};
use crate::log::Level;
use crate::tui::state::RenderState;

pub fn draw(f: &mut Frame<'_>, area: Rect, state: &RenderState) {
    let theme = state.settings.theme.clone();
    let outer = theme.block().title(" Live Logs ");

    let records = crate::log::ring()
    .map(|r| r.snapshot())
    .unwrap_or_default();

    let lines: Vec<Line> = records
    .iter()
    .map(|rec| {
        let color = match rec.level {
            Level::Error => Color::Red,
            Level::Warn  => Color::Yellow,
            Level::Info  => Color::Gray,
            Level::Debug | Level::Trace => Color::DarkGray,
        };
        let prefix = format!("[{} {}] ", rec.level.label(), rec.category.slug());
        Line::from(vec![
            Span::styled(
                prefix,
                Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
            ),
            Span::styled(rec.message.clone(), Style::default().fg(color)),
        ])
    })
    .collect();

    let paragraph = Paragraph::new(lines)
    .block(outer)
    .wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);
}
