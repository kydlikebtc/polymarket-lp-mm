use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph};

use crate::tui::app::App;

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Min(0),   // Chart
        Constraint::Length(3), // Status bar
    ])
    .split(area);

    render_price_chart(app, frame, chunks[0]);
    render_status_bar(app, frame, chunks[1]);
}

fn render_price_chart(app: &App, frame: &mut Frame, area: Rect) {
    let snap = match &app.snapshot {
        Some(s) => s,
        None => {
            frame.render_widget(
                Paragraph::new("Waiting for price data...")
                    .block(Block::default().borders(Borders::ALL).title("Price Chart")),
                area,
            );
            return;
        }
    };

    if snap.price_histories.is_empty() {
        frame.render_widget(
            Paragraph::new("No price history available")
                .block(Block::default().borders(Borders::ALL).title("Price Chart")),
            area,
        );
        return;
    }

    let idx = app.selected_market_index.min(snap.price_histories.len() - 1);
    let history = &snap.price_histories[idx];

    if history.points.is_empty() {
        frame.render_widget(
            Paragraph::new("No data points")
                .block(Block::default().borders(Borders::ALL).title("Price Chart")),
            area,
        );
        return;
    }

    // Compute bounds
    let (x_min, x_max) = history
        .points
        .iter()
        .fold((f64::MAX, f64::MIN), |(min, max), (x, _)| {
            (min.min(*x), max.max(*x))
        });
    let (y_min, y_max) = history
        .points
        .iter()
        .fold((f64::MAX, f64::MIN), |(min, max), (_, y)| {
            (min.min(*y), max.max(*y))
        });

    // Add padding to Y axis
    let y_range = (y_max - y_min).max(0.01);
    let y_lo = (y_min - y_range * 0.1).max(0.0);
    let y_hi = y_max + y_range * 0.1;

    let dataset = Dataset::default()
        .name(history.market_id.as_str())
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Cyan))
        .data(&history.points);

    let title = format!(
        "Price: {} ({} points)",
        history.market_id,
        history.points.len()
    );

    let chart = Chart::new(vec![dataset])
        .block(Block::default().borders(Borders::ALL).title(title))
        .x_axis(
            Axis::default()
                .title("Time (60min window)")
                .bounds([x_min, x_max])
                .labels(vec![Line::from("60m ago"), Line::from("now")]),
        )
        .y_axis(
            Axis::default()
                .title("Price")
                .bounds([y_lo, y_hi])
                .labels(vec![
                    Line::from(format!("{:.3}", y_lo)),
                    Line::from(format!("{:.3}", (y_lo + y_hi) / 2.0)),
                    Line::from(format!("{:.3}", y_hi)),
                ]),
        );

    frame.render_widget(chart, area);
}

fn render_status_bar(app: &App, frame: &mut Frame, area: Rect) {
    let market_count = app
        .snapshot
        .as_ref()
        .map(|s| s.price_histories.len())
        .unwrap_or(0);

    let line = Line::from(vec![
        Span::raw(format!(
            " Market {}/{} ",
            app.selected_market_index + 1,
            market_count
        )),
        Span::styled(
            "| [<-/->] Switch Market  ",
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw("[1-4] Tab  [q] Quit"),
    ]);

    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
        area,
    );
}
