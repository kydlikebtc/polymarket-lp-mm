use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::risk::RiskLevel;
use crate::tui::app::App;

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(3), // Header
        Constraint::Min(0),   // Market table
        Constraint::Length(3), // Footer (PnL)
    ])
    .split(area);

    render_header(app, frame, chunks[0]);
    render_market_table(app, frame, chunks[1]);
    render_footer(app, frame, chunks[2]);
}

fn render_header(app: &App, frame: &mut Frame, area: Rect) {
    let snap = match &app.snapshot {
        Some(s) => s,
        None => {
            frame.render_widget(
                Paragraph::new("Waiting for data...")
                    .block(Block::default().borders(Borders::ALL).title("Status")),
                area,
            );
            return;
        }
    };

    let uptime = chrono::Utc::now() - app.started_at;
    let uptime_str = format!(
        "{}h {}m",
        uptime.num_hours(),
        uptime.num_minutes() % 60,
    );

    let risk_style = risk_color(snap.risk_level);
    let ws_icon = if snap.market_ws_connected && snap.user_ws_connected {
        Span::styled("WS:OK", Style::default().fg(Color::Green))
    } else {
        Span::styled(
            format!("WS:DOWN({}s)", snap.ws_disconnect_secs),
            Style::default().fg(Color::Red),
        )
    };

    let header = Line::from(vec![
        Span::styled(
            format!(" Polymarket MM v{} ", env!("CARGO_PKG_VERSION")),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("| "),
        Span::styled(format!("{}", snap.risk_level), risk_style),
        Span::raw(" | "),
        ws_icon,
        Span::raw(" | "),
        Span::raw(format!("Up: {uptime_str}")),
        Span::raw(" "),
    ]);

    frame.render_widget(
        Paragraph::new(header).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn render_market_table(app: &App, frame: &mut Frame, area: Rect) {
    let snap = match &app.snapshot {
        Some(s) => s,
        None => return,
    };

    let header = Row::new(vec![
        Cell::from("Market"),
        Cell::from("Mid"),
        Cell::from("Spread"),
        Cell::from("Bid"),
        Cell::from("Ask"),
        Cell::from("IIR"),
        Cell::from("YES"),
        Cell::from("NO"),
        Cell::from("Value"),
        Cell::from("Orders"),
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = snap
        .markets
        .iter()
        .map(|m| {
            let iir_style = if m.iir.abs() >= rust_decimal_macros::dec!(0.5) {
                Style::default().fg(Color::Red)
            } else if m.iir.abs() >= rust_decimal_macros::dec!(0.3) {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Green)
            };

            Row::new(vec![
                Cell::from(truncate_str(&m.name, 20)),
                Cell::from(format!("{:.4}", m.midpoint)),
                Cell::from(format!("{:.4}", m.spread)),
                Cell::from(
                    m.best_bid
                        .map(|b| format!("{:.4}", b))
                        .unwrap_or_else(|| "-".to_string()),
                ),
                Cell::from(
                    m.best_ask
                        .map(|a| format!("{:.4}", a))
                        .unwrap_or_else(|| "-".to_string()),
                ),
                Cell::from(format!("{:.3}", m.iir)).style(iir_style),
                Cell::from(format!("{:.1}", m.yes_shares)),
                Cell::from(format!("{:.1}", m.no_shares)),
                Cell::from(format!("${:.2}", m.yes_value + m.no_value)),
                Cell::from(format!("{}", m.active_orders)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Min(20),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(7),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Markets"));

    frame.render_widget(table, area);
}

fn render_footer(app: &App, frame: &mut Frame, area: Rect) {
    let snap = match &app.snapshot {
        Some(s) => s,
        None => return,
    };

    let pnl_color = if snap.daily_pnl >= rust_decimal_macros::dec!(0) {
        Color::Green
    } else {
        Color::Red
    };

    let footer = Line::from(vec![
        Span::raw(" PnL: "),
        Span::styled(
            format!("${:.2}", snap.daily_pnl),
            Style::default().fg(pnl_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" | Fills: {} ", snap.fill_count)),
        Span::raw("| [1-4] Tab  [q] Quit"),
    ]);

    frame.render_widget(
        Paragraph::new(footer).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn risk_color(level: RiskLevel) -> Style {
    match level {
        RiskLevel::L1Normal => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        RiskLevel::L2Warning => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        RiskLevel::L3Emergency => Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD),
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}
