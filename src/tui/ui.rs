use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Tabs;

use super::app::{App, Tab};
use super::tabs;

/// Top-level render: tab bar + active tab content
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
    }
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
