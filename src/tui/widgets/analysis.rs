use ratatui::{
    layout::{Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::player::analysis::AnalysisReport;
use crate::tui::theme::{Theme, Token};

pub fn draw(f: &mut Frame<'_>, area: Rect, theme: &Theme, line_idx: usize, report: &AnalysisReport) {
    // Fixed size, taller to accommodate multiple explanations and intervals
    let w = 76u16.min(area.width.saturating_sub(4));
    let h = 34u16.min(area.height.saturating_sub(4));

    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);

    let help_bg = theme.color(Token::HelpBg);
    let key_style = Style::default().fg(theme.color(Token::HelpKey)).bg(help_bg).add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.color(Token::HelpFg)).bg(help_bg);
    let dim_style = Style::default().fg(theme.color(Token::HelpDim)).bg(help_bg);
    let header_style = Style::default().fg(theme.color(Token::HelpKey)).bg(help_bg).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);

    let mut lines: Vec<Line> = Vec::new();

    // ── Active Instruments ──
    lines.push(Line::from(Span::styled(" ACTIVE INSTRUMENTS", header_style)));
    if report.active_parts.is_empty() {
        lines.push(Line::from(Span::styled("   (No notes playing at playhead)", dim_style)));
    } else {
        for (name, notes) in &report.active_parts {
            lines.push(Line::from(vec![
                Span::styled(format!("   {:<16} ", name), key_style),
                                  Span::styled(notes.join(", "), desc_style),
            ]));
        }
    }
    lines.push(Line::from(""));

    // ── Context ──
    lines.push(Line::from(Span::styled(" CONTEXT", header_style)));
    lines.push(Line::from(vec![
        Span::styled("   Scale      ", key_style),
                          Span::styled(&report.scale, desc_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled("   Contour    ", key_style),
                          Span::styled(&report.contour, desc_style),
    ]));
    if let Some(m) = &report.motif {
        lines.push(Line::from(vec![
            Span::styled("   Motif      ", key_style),
                              Span::styled(m, desc_style),
        ]));
    }
    lines.push(Line::from(""));

    // ── Harmony ──
    lines.push(Line::from(Span::styled(" HARMONY", header_style)));
    lines.push(Line::from(vec![
        Span::styled("   Root       ", key_style),
                          Span::styled(&report.root, desc_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled("   Chord      ", key_style),
                          Span::styled(&report.chord, Style::default().fg(Color::Green).bg(help_bg).add_modifier(Modifier::BOLD)),
    ]));

    if !report.intervals.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("   Intervals  ", key_style),
                              Span::styled(report.intervals.join(", "), desc_style),
        ]));
    }
    lines.push(Line::from(""));

    // ── Chromatic Grid ──
    lines.push(Line::from(Span::styled(" CHROMATIC GRID", header_style)));
    lines.push(Line::from(vec![
        Span::raw("   "),
                          Span::styled(&report.chromatic_grid, Style::default().fg(Color::Cyan).bg(help_bg)),
    ]));
    lines.push(Line::from(Span::styled("   C  C# D  D# E  F  F# G  G# A  A# B", dim_style)));
    lines.push(Line::from(""));

    // ── The "Why" ──
    lines.push(Line::from(Span::styled(" WHY DOES IT FEEL LIKE THIS?", header_style)));
    for reason in &report.explanations {
        lines.push(Line::from(vec![
            Span::styled("   • ", Style::default().fg(Color::Yellow).bg(help_bg)),
                              Span::styled(reason, Style::default().fg(Color::Yellow).bg(help_bg).add_modifier(Modifier::ITALIC)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(" Press [Esc] or [A] to close", dim_style)));

    let block = Block::default()
    .borders(Borders::ALL)
    .border_set(theme.border_set())
    .border_style(Style::default().fg(theme.color(Token::HelpKey)))
    .title(format!(" Analysis: Line {} ", line_idx + 1))
    .style(Style::default().bg(help_bg));

    let paragraph = Paragraph::new(lines)
    .block(block)
    .wrap(Wrap { trim: true });

    f.render_widget(paragraph, rect);
}
