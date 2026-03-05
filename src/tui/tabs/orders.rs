use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::tui::app::{App, OrderFilter};

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Min(0),   // Order table
        Constraint::Length(3), // Status bar
    ])
    .split(area);

    render_order_table(app, frame, chunks[0]);
    render_status_bar(app, frame, chunks[1]);
}

fn render_order_table(app: &App, frame: &mut Frame, area: Rect) {
    let snap = match &app.snapshot {
        Some(s) => s,
        None => {
            frame.render_widget(
                Paragraph::new("No orders data")
                    .block(Block::default().borders(Borders::ALL).title("Orders")),
                area,
            );
            return;
        }
    };

    let orders: Vec<_> = snap
        .orders
        .iter()
        .filter(|o| match app.order_filter {
            OrderFilter::All => true,
            OrderFilter::LiveOnly => o.status == "Live",
        })
        .collect();

    let header = Row::new(vec![
        Cell::from("Order ID"),
        Cell::from("Market"),
        Cell::from("Side"),
        Cell::from("Price"),
        Cell::from("Size"),
        Cell::from("Status"),
        Cell::from("Age"),
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let visible_rows = (area.height as usize).saturating_sub(4); // borders + header
    let total = orders.len();
    let offset = app.order_scroll.min(total.saturating_sub(visible_rows));

    let rows: Vec<Row> = orders
        .iter()
        .skip(offset)
        .take(visible_rows)
        .map(|o| {
            let side_style = if o.side == "BUY" {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            };
            let status_style = match o.status.as_str() {
                "Live" => Style::default().fg(Color::Cyan),
                "Pending" => Style::default().fg(Color::Yellow),
                "Matched" => Style::default().fg(Color::Green),
                "Canceled" => Style::default().fg(Color::DarkGray),
                _ => Style::default(),
            };

            Row::new(vec![
                Cell::from(truncate_id(&o.order_id)),
                Cell::from(o.market_id.chars().take(12).collect::<String>()),
                Cell::from(o.side.clone()).style(side_style),
                Cell::from(format!("{:.4}", o.price)),
                Cell::from(format!("{:.1}", o.size)),
                Cell::from(o.status.clone()).style(status_style),
                Cell::from(format_age(o.age_secs)),
            ])
        })
        .collect();

    let title = format!(
        "Orders ({}/{}) [{}]",
        rows.len().min(total),
        total,
        match app.order_filter {
            OrderFilter::All => "All",
            OrderFilter::LiveOnly => "Live",
        }
    );

    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(title));

    frame.render_widget(table, area);
}

fn render_status_bar(app: &App, frame: &mut Frame, area: Rect) {
    let filter_label = match app.order_filter {
        OrderFilter::All => "All",
        OrderFilter::LiveOnly => "Live",
    };

    let line = Line::from(vec![
        Span::raw(format!(" Filter: {filter_label} ")),
        Span::raw("| [j/k] Scroll  [f] Filter  [1-4] Tab  [q] Quit"),
    ]);

    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn truncate_id(id: &str) -> String {
    if id.len() <= 10 {
        id.to_string()
    } else {
        format!("{}..{}", &id[..4], &id[id.len() - 4..])
    }
}

fn format_age(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}
