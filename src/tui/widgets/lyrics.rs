use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Padding, Paragraph, Wrap},
    Frame,
};
use std::sync::atomic::Ordering;

use crate::player::{Project, Transport};

pub fn draw(f: &mut Frame<'_>, area: Rect, tp: &Transport, project: &Project) {
    if project.lyrics.is_empty() {
        return;
    }

    let pos = tp.position.load(Ordering::Relaxed).max(0) as f64;
    let secs = pos / project.sample_rate as f64;

    // Find the active lyric line
    let mut active_line = None;
    for line in &project.lyrics {
        if line.start_sec <= secs && secs < line.end_sec {
            active_line = Some(line);
            break;
        }
    }

    let Some(line) = active_line else {
        return;
    };

    // Concatenate only the segments that have been played so far
    let mut raw_text = String::new();
    for seg in &line.segments {
        if seg.time_sec <= secs {
            raw_text.push_str(&seg.text);
        }
    }

    if raw_text.trim().is_empty() {
        return;
    }

    // Position: Centered horizontally, placed in the lower third
    let area_w = area.width.min(70);
    let area_h = 3;
    let x = area.x + (area.width.saturating_sub(area_w)) / 2;
    let y = area.y + (area.height / 2) + (area.height / 4) - 1;

    let rect = Rect { x, y, width: area_w, height: area_h };

    // 1. Clear the background (punches a hole through the tracker/cover)
    f.render_widget(Clear, rect);

    // 2. Parse the Markdown into styled Spans
    let spans = parse_markdown(&raw_text);
    let text_line = Line::from(spans);

    // 3. Draw the subtitle box
    let block = Block::default()
    .style(Style::default().bg(Color::Black))
    .padding(Padding::horizontal(2));

    let main_text = Paragraph::new(text_line)
    .style(Style::default().bg(Color::Black))
    .alignment(Alignment::Center)
    .wrap(Wrap { trim: true })
    .block(block);

    f.render_widget(main_text, rect);
}

/// A tiny Markdown parser for lyric formatting.
/// Supports:
///   `**bold**`
///   `*italic*`
///   `{red}text{/}` (colors: red, blue, green, yellow, magenta, cyan, white, gray)
///   `\*` (escape a literal asterisk)
fn parse_markdown(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut current_text = String::new();

    // Default style: LightGreen, NOT bold, NOT italic
    let mut current_color = Color::LightGreen;
    let mut is_bold = false;
    let mut is_italic = false;

    let mut chars = text.chars().peekable();

    let build_style = |color: Color, bold: bool, italic: bool| {
        // If it's bold, force it to White so it really pops against the green
        let final_color = if bold { Color::White } else { color };
        let mut s = Style::default().fg(final_color).bg(Color::Black);
        if bold { s = s.add_modifier(Modifier::BOLD); }
        if italic { s = s.add_modifier(Modifier::ITALIC); }
        s
    };

    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if next == '*' || next == '{' {
                    current_text.push(next);
                    chars.next();
                    continue;
                }
            }
            current_text.push(c);
        } else if c == '*' {
            if chars.peek() == Some(&'*') {
                // Bold toggle
                chars.next();
                if !current_text.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current_text), build_style(current_color, is_bold, is_italic)));
                }
                is_bold = !is_bold;
            } else {
                // Italic toggle
                if !current_text.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current_text), build_style(current_color, is_bold, is_italic)));
                }
                is_italic = !is_italic;
            }
        } else if c == '{' {
            let mut tag = String::new();
            let mut temp_chars = chars.clone();
            let mut valid_tag = false;

            while let Some(&next_c) = temp_chars.peek() {
                if next_c == '}' {
                    valid_tag = true;
                    break;
                }
                tag.push(temp_chars.next().unwrap());
            }

            if valid_tag {
                if !current_text.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current_text), build_style(current_color, is_bold, is_italic)));
                }
                for _ in 0..tag.len() + 1 {
                    chars.next();
                }

                current_color = if tag == "/" {
                    Color::LightGreen
                } else {
                    match tag.to_lowercase().as_str() {
                        "red" => Color::Red,
                        "blue" => Color::Blue,
                        "green" => Color::Green,
                        "yellow" => Color::Yellow,
                        "magenta" | "pink" => Color::Magenta,
                        "cyan" => Color::Cyan,
                        "white" => Color::White,
                        "gray" | "grey" => Color::Gray,
                        _ => Color::LightGreen,
                    }
                };
            } else {
                current_text.push(c);
            }
        } else {
            current_text.push(c);
        }
    }

    if !current_text.is_empty() {
        spans.push(Span::styled(current_text, build_style(current_color, is_bold, is_italic)));
    }

    spans
}
