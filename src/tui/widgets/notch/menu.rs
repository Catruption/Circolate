use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use crate::tui::state::{MenuOption, MenuSubmenu, RenderState, MENU_OPTIONS};

pub fn draw(f: &mut Frame<'_>, area: Rect, state: &mut RenderState) {
    let theme = state.settings.theme.clone();
    let title = match state.menu.submenu {
        None => " Idle - Select an Option ",
        Some(MenuSubmenu::Directories) => " Idle - Directories ",
    };
    let outer = theme.block().title(title);
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    if inner.height == 0 {
        return;
    }

    // White background for clickable options
    let clickable_style = Style::default()
    .fg(Color::Black)
    .bg(Color::White)
    .add_modifier(Modifier::BOLD);

    // Light green text with a visible insertion point while editing.
    let editable_style = Style::default()
    .fg(Color::LightGreen)
    .bg(Color::Reset);

    let dim_style = Style::default().fg(Color::DarkGray);

    let action = |index: usize, option: &MenuOption| {
        let label = match state.menu.arg_label(index) {
            Some(arg) => format!("{} [{}]", option.label, arg),
            None => option.label.to_string(),
        };
        Line::from(Span::styled(
            format!(" {:<22}", label),
            if state.menu.selected_index == index { clickable_style } else { Style::default() },
        ))
    };
    let mut lines: Vec<Line<'_>> = MENU_OPTIONS
    .iter()
    .enumerate()
    .map(|(index, option)| action(index, option))
    .collect();

    let hint = if state.menu.edit_mode.is_some() {
        "Left/Right cursor  Home/End jump  Enter/Esc save"
    } else if state.menu.submenu.is_some() {
        "Enter edit user root  PgUp/PgDn or [/] scroll  Esc back"
    } else {
        "Up/Down choose  Left/Right cycle args  Enter run"
    };
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(hint, dim_style)),
    ]);

    if let Some(status) = &state.menu.status {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(status.clone(), clickable_style)));
    }

    if inner.height < 8 {
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let top_height = (lines.len() as u16).min(inner.height - 4);
    let chunks = Layout::vertical([
        Constraint::Length(top_height),
        Constraint::Min(1),
    ]).split(inner);

    f.render_widget(Paragraph::new(lines), chunks[0]);

    match state.menu.submenu {
        None => {
            // The main idle screen uses this space for the live log bus.
            super::logs::draw(f, chunks[1], state);
        }
        Some(MenuSubmenu::Directories) => {
            let mut dir_lines = vec![
                Line::from(Span::styled(
                    "One user root; its subfolders mirror the localshare tree:",
                    dim_style,
                )),
                Line::from(Span::styled(
                    "plugins/ soundfonts/ halotexts/ instruments/ samples/ patches/ covers/",
                    dim_style,
                )),
                Line::from(""),
            ];

            for (i, dir) in state.menu.directories.iter().enumerate() {
                let is_editing = state.menu.edit_mode == Some(i);
                let indicator = if is_editing { "> " } else { "  " };
                let style = if is_editing { editable_style } else { Style::default() };
                let kind = state.menu.directory_kinds.get(i).copied().unwrap_or("other");

                if is_editing {
                    // Highlight the character under the insertion point so
                    // Left/Right/Home/End have visible feedback.
                    let cursor = state.menu.edit_cursor;
                    let before: String = dir.chars().take(cursor).collect();
                    let at = dir.chars().nth(cursor).unwrap_or(' ').to_string();
                    let after: String = dir.chars().skip(cursor + 1).collect();
                    dir_lines.push(Line::from(vec![
                        Span::styled(format!("{}{:<12}", indicator, kind), style),
                        Span::styled(before, style),
                        Span::styled(at, clickable_style),
                        Span::styled(after, style),
                    ]));
                } else {
                    dir_lines.push(Line::from(Span::styled(
                        format!("{}{:<12}{}", indicator, kind, dir),
                        style,
                    )));
                }
            }

            dir_lines.extend([
                Line::from(""),
                Line::from(Span::styled(
                    "These paths are always checked: drop files here directly.",
                    dim_style,
                )),
            ]);
            for root in &state.menu.system_roots {
                let marker = if root.exists { "ok" } else { "--" };
                dir_lines.push(Line::from(Span::styled(
                    format!(" {:<9}{} {}", root.kind, marker, root.path.display()),
                    dim_style,
                )));
            }

            // These paths come from the same resolvers the player uses, so
            // they are valid drop locations even without a user root.
            let block = theme.block().title(" Directories - Esc back ");
            let body = block.inner(chunks[1]);
            let max_scroll = dir_lines.len().saturating_sub(body.height as usize);
            let scroll = state.menu.system_scroll.min(max_scroll);
            state.menu.system_scroll = scroll;

            f.render_widget(block, chunks[1]);
            f.render_widget(Paragraph::new(dir_lines).scroll((scroll as u16, 0)), body);
        }
    }
}
