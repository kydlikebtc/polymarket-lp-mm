use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// Render a centered modal overlay with title, content, and key hints.
/// The modal clears the background behind it for contrast.
pub fn render_modal(
    frame: &mut Frame,
    title: &str,
    content_lines: Vec<Line<'_>>,
    hints: &str,
    width_pct: u16,
    height: u16,
) {
    let area = frame.area();
    let modal_area = centered_rect(width_pct, height, area);

    // Clear background behind modal
    frame.render_widget(Clear, modal_area);

    // Modal block with title and border
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(Line::from(Span::styled(
            format!(" {hints} "),
            Style::default().fg(Color::DarkGray),
        )))
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    // Render content lines
    let paragraph = Paragraph::new(content_lines);
    frame.render_widget(paragraph, inner);
}

/// Render a selection list inside a modal (for profile selection, search results, etc.)
pub fn render_selection_modal(
    frame: &mut Frame,
    title: &str,
    items: &[String],
    selected: usize,
    hints: &str,
    width_pct: u16,
) {
    let height = (items.len() as u16 + 4).min(20); // +4 for borders + padding

    let lines: Vec<Line<'_>> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let indicator = if i == selected { ">" } else { " " };
            let style = if i == selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(format!("{indicator} {item}"), style))
        })
        .collect();

    render_modal(frame, title, lines, hints, width_pct, height);
}

/// Render a confirmation dialog with Yes/No.
pub fn render_confirm_modal(
    frame: &mut Frame,
    title: &str,
    message: &str,
    confirm_selected: bool,
) {
    let yes_style = if confirm_selected {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let no_style = if !confirm_selected {
        Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let lines = vec![
        Line::from(Span::raw(format!(" {message}"))),
        Line::from(""),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(" [Y] Yes ", yes_style),
            Span::raw("   "),
            Span::styled(" [N] No ", no_style),
        ]),
    ];

    render_modal(frame, title, lines, "[Y/N] Select  [Esc] Cancel", 50, 7);
}

/// Calculate a centered rect within the given area.
fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = (area.width * percent_x / 100).min(area.width);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let h = height.min(area.height);

    Rect::new(x, y, width, h)
}
