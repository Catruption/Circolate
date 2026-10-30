use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::player::analysis::AnalysisReport;
use crate::tui::theme::{Theme, Token};

const EXPLANATION_SLOTS: usize = 4;

pub fn draw(f: &mut Frame<'_>, area: Rect, theme: &Theme, line_idx: usize, report: &AnalysisReport) {
    let w = (area.width * 85 / 100).min(100).min(area.width.saturating_sub(4));
    let h = (area.height * 80 / 100).min(40).min(area.height.saturating_sub(4));

    let x = area.x + (area.width.saturating_sub(w)) / 2;
    
    // Push the modal down slightly so it doesn't obscure the top of the tracker
    let y = area.y + (area.height.saturating_sub(h) * 6) / 10;
    let y = y.min(area.y + area.height.saturating_sub(h));
    
    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);

    let help_bg = theme.color(Token::HelpBg);
    let key_style = Style::default().fg(Color::Magenta).bg(help_bg).add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.color(Token::HelpFg)).bg(help_bg);
    let dim_style = Style::default().fg(theme.color(Token::HelpDim)).bg(help_bg);
    let header_style = Style::default().fg(Color::Magenta).bg(help_bg).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);

    let mut lines: Vec<Line> = Vec::new();

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
    let motif_text = report.motif.as_deref().unwrap_or("—");
    lines.push(Line::from(vec![
        Span::styled("   Motif      ", key_style),
        Span::styled(motif_text, desc_style),
    ]));
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
    
    let intervals_str = if report.intervals.is_empty() {
        "—".to_string()
    } else if report.intervals.len() > 5 {
        format!("{} (+{} more)", report.intervals[..5].join(", "), report.intervals.len() - 5)
    } else {
        report.intervals.join(", ")
    };
    lines.push(Line::from(vec![
        Span::styled("   Intervals  ", key_style),
        Span::styled(intervals_str, desc_style),
    ]));
    lines.push(Line::from(""));

    // ── Motion ──
    lines.push(Line::from(Span::styled(" MOTION", header_style)));
    lines.push(Line::from(vec![
        Span::styled("   Anchor     ", key_style),
        Span::styled(if report.motion_anchor { "Yes (static pedal)" } else { "—" }, if report.motion_anchor { Style::default().fg(Color::Yellow).bg(help_bg) } else { desc_style }),
    ]));
    
    let leap_str = if report.motion_leap == 0 {
        "—".to_string()
    } else if report.motion_leap > 0 {
        format!("+{} semitones", report.motion_leap)
    } else {
        format!("{} semitones", report.motion_leap)
    };
    lines.push(Line::from(vec![
        Span::styled("   Leap       ", key_style),
        Span::styled(leap_str, desc_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled("   Register   ", key_style),
        Span::styled(&report.motion_register, desc_style),
    ]));
    lines.push(Line::from(""));

    // ── Chromatic Grid ──
    lines.push(Line::from(Span::styled(" CHROMATIC GRID", header_style)));
    lines.push(Line::from(vec![
        Span::raw("   "),
        Span::styled(&report.chromatic_grid, Style::default().fg(Color::Cyan).bg(help_bg)),
    ]));
    lines.push(Line::from(Span::styled("   C  C# D  D# E  F  F# G  G# A  A# B", dim_style)));
    lines.push(Line::from(""));

    // ── Why (fixed 4 slots) ──
    lines.push(Line::from(Span::styled(" WHY DOES IT FEEL LIKE THIS?", header_style)));
    let expl_count = report.explanations.len().min(EXPLANATION_SLOTS);
    for i in 0..expl_count {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("   • ", Style::default().fg(Color::Yellow).bg(help_bg)),
            Span::styled(&report.explanations[i], Style::default().fg(Color::Yellow).bg(help_bg).add_modifier(Modifier::ITALIC)),
        ]));
    }
    for _ in expl_count..EXPLANATION_SLOTS {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("   ", dim_style)));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set())
        .border_style(Style::default().fg(Color::Magenta))
        .title(format!(" Analysis: Line {} ", line_idx + 1))
        .style(Style::default().bg(help_bg));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(block.inner(rect));

    f.render_widget(block, rect);
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
    f.render_widget(paragraph, chunks[0]);

    let footer = Paragraph::new(" Press [Esc] or [A] to close")
        .style(dim_style)
        .alignment(ratatui::layout::Alignment::Center);
    f.render_widget(footer, chunks[1]);
}