use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use crate::tui::state::RenderState;

pub fn draw(f: &mut Frame<'_>, area: Rect, state: &mut RenderState) {
    let theme = state.settings.theme.clone();
    let outer = theme.block().title(" Idle - Select an Option ");

    // White background for clickable options
    let clickable_style = Style::default()
    .fg(Color::Black)
    .bg(Color::White)
    .add_modifier(Modifier::BOLD);

    // Black background (reset) for editable text
    let editable_style = Style::default()
    .fg(Color::LightGreen)
    .bg(Color::Reset);

    // Dim for uninteractive
    let dim_style = Style::default().fg(Color::DarkGray);

    let mut lines = vec![
        Line::from(Span::styled(" PLAY        ", if state.menu.selected_index == 0 { clickable_style } else { Style::default() })),
        Line::from(Span::styled(" DUMP        ", if state.menu.selected_index == 1 { clickable_style } else { Style::default() })),
        Line::from(Span::styled(" EXPORT      ", if state.menu.selected_index == 2 { clickable_style } else { Style::default() })),
        Line::from(Span::styled(" DIRECTORIES ", if state.menu.selected_index == 3 { clickable_style } else { Style::default() })),
        Line::from(""),
        Line::from(Span::styled("Paths (Editable):", dim_style)),
    ];

    for (i, dir) in state.menu.directories.iter().enumerate() {
        let is_editing = state.menu.edit_mode == Some(i);
        let indicator = if is_editing { "> " } else { "  " };
        let style = if is_editing { editable_style } else { Style::default() };
        lines.push(Line::from(Span::styled(format!("{}{}", indicator, dir), style)));
    }

    let paragraph = Paragraph::new(lines).block(outer);
    f.render_widget(paragraph, area);
}
