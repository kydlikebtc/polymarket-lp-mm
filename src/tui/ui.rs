use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Tabs;

use super::app::{App, ModalState, Tab};
use super::modal;
use super::tabs;

/// Top-level render: tab bar + active tab content + modal overlay
pub fn render(app: &App, frame: &mut Frame) {
    let chunks = Layout::vertical([
        Constraint::Length(3), // Tab bar
        Constraint::Min(0),   // Content
    ])
    .split(frame.area());

    render_tab_bar(app, frame, chunks[0]);

    match app.current_tab {
        Tab::Overview => tabs::overview::render(app, frame, chunks[1]),
        Tab::Orders => tabs::orders::render(app, frame, chunks[1]),
        Tab::Risk => tabs::risk::render(app, frame, chunks[1]),
        Tab::Charts => tabs::charts::render(app, frame, chunks[1]),
        Tab::Strategy => tabs::strategy::render(app, frame, chunks[1]),
    }

    // Modal overlay — rendered on top of everything
    render_modal_overlay(app, frame);
}

fn render_tab_bar(app: &App, frame: &mut Frame, area: Rect) {
    let titles: Vec<Span> = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let num = i + 1;
            if *tab == app.current_tab {
                Span::styled(
                    format!(" {num}:{} ", tab.title()),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    format!(" {num}:{} ", tab.title()),
                    Style::default().fg(Color::DarkGray),
                )
            }
        })
        .collect();

    let tabs = Tabs::new(titles)
        .select(app.current_tab.index())
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .divider("|");

    frame.render_widget(tabs, area);
}

/// Render the active modal overlay, if any.
fn render_modal_overlay(app: &App, frame: &mut Frame) {
    match &app.modal {
        ModalState::None => {}
        ModalState::EditParams {
            market_name,
            fields,
            cursor,
            editing,
            ..
        } => {
            let height = (fields.len() as u16 + 4).min(16);
            let mut lines: Vec<Line<'_>> = Vec::new();
            lines.push(Line::from(Span::styled(
                format!(" Market: {market_name}"),
                Style::default().fg(Color::Cyan),
            )));
            lines.push(Line::from(""));

            for (i, field) in fields.iter().enumerate() {
                let is_editing = editing.is_some() && i == *cursor;
                if is_editing {
                    if let Some(input) = editing {
                        let cursor_style = Style::default().fg(Color::Black).bg(Color::Yellow);
                        let (before, after) = input.value.split_at(input.cursor.min(input.value.len()));
                        let cursor_char = after.chars().next().unwrap_or(' ');
                        let rest = if !after.is_empty() && after.len() > cursor_char.len_utf8() {
                            &after[cursor_char.len_utf8()..]
                        } else {
                            ""
                        };
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  > {}: ", field.label),
                                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(before, Style::default().fg(Color::Yellow)),
                            Span::styled(cursor_char.to_string(), cursor_style),
                            Span::styled(rest, Style::default().fg(Color::Yellow)),
                        ]));
                    }
                } else {
                    let indicator = if i == *cursor { ">" } else { " " };
                    let style = if i == *cursor {
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {indicator} {}: ", field.label),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(&field.value, style),
                    ]));
                }
            }

            modal::render_modal(
                frame,
                " Edit Parameters ",
                lines,
                "[j/k] Navigate  [Enter] Edit  [s] Save  [Esc] Cancel",
                60,
                height + 2,
            );
        }
        ModalState::SelectProfile {
            profiles, cursor, ..
        } => {
            modal::render_selection_modal(
                frame,
                " Select Profile ",
                profiles,
                *cursor,
                "[j/k] Navigate  [Enter] Select  [Esc] Cancel",
                50,
            );
        }
        ModalState::Confirm {
            title,
            message,
            selected_yes,
            ..
        } => {
            modal::render_confirm_modal(frame, title, message, *selected_yes);
        }
        ModalState::SearchMarket {
            query,
            results,
            cursor,
            searching,
        } => {
            let mut lines: Vec<Line<'_>> = Vec::new();

            // Search input line
            lines.push(Line::from(vec![
                Span::styled(" Search: ", Style::default().fg(Color::Cyan)),
                Span::styled(&query.value, Style::default().fg(Color::Yellow)),
                Span::styled("_", Style::default().fg(Color::Yellow).add_modifier(Modifier::SLOW_BLINK)),
            ]));
            lines.push(Line::from(""));

            if *searching {
                lines.push(Line::from(Span::styled(
                    " Searching...",
                    Style::default().fg(Color::DarkGray),
                )));
            } else if results.is_empty() {
                lines.push(Line::from(Span::styled(
                    " Type query and press Enter to search",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                for (i, result) in results.iter().enumerate() {
                    let indicator = if i == *cursor { ">" } else { " " };
                    let style = if i == *cursor {
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };

                    let mid_str = result
                        .midpoint
                        .map(|m| format!(" mid={:.2}", m))
                        .unwrap_or_default();

                    let question_display = if result.question.len() > 50 {
                        format!("{}...", &result.question[..47])
                    } else {
                        result.question.clone()
                    };

                    lines.push(Line::from(Span::styled(
                        format!(" {indicator} {question_display}{mid_str}"),
                        style,
                    )));
                }
            }

            let height = (lines.len() as u16 + 4).min(20);
            modal::render_modal(
                frame,
                " Add Market ",
                lines,
                "[Enter] Search/Add  [j/k] Navigate  [Esc] Cancel",
                70,
                height,
            );
        }
    }
}
