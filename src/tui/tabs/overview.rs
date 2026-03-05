use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::risk::RiskLevel;
use crate::tui::app::App;
use crate::tui::snapshot::DashboardSnapshot;

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
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

    let chunks = Layout::vertical([
        Constraint::Length(3),  // Header bar
        Constraint::Length(6),  // Account + Strategy panels (side by side)
        Constraint::Min(0),    // Market table
        Constraint::Length(6),  // Pricing + Execution panels (side by side)
        Constraint::Length(3),  // Footer
    ])
    .split(area);

    render_header(app, snap, frame, chunks[0]);
    render_info_panels(snap, frame, chunks[1]);
    render_market_table(snap, frame, chunks[2]);
    render_factor_panels(snap, frame, chunks[3]);
    render_footer(snap, frame, chunks[4]);
}

fn render_header(app: &App, snap: &DashboardSnapshot, frame: &mut Frame, area: Rect) {
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

/// Render Account + Strategy info panels side by side
fn render_info_panels(snap: &DashboardSnapshot, frame: &mut Frame, area: Rect) {
    let cols = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .split(area);

    render_account_panel(snap, frame, cols[0]);
    render_strategy_panel(snap, frame, cols[1]);
}

fn render_account_panel(snap: &DashboardSnapshot, frame: &mut Frame, area: Rect) {
    // Sum up YES/NO token values across all markets
    let total_yes: rust_decimal::Decimal = snap.markets.iter().map(|m| m.yes_value).sum();
    let total_no: rust_decimal::Decimal = snap.markets.iter().map(|m| m.no_value).sum();
    let total_value = snap.usdc_balance + total_yes + total_no;

    let lines = vec![
        Line::from(vec![
            Span::styled(" USDC:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("${:.2}", snap.usdc_balance),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Tokens:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" Y=${:.2}  N=${:.2}", total_yes, total_no)),
        ]),
        Line::from(vec![
            Span::styled(" Total: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("${:.2}", total_value),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Account")),
        area,
    );
}

fn render_strategy_panel(snap: &DashboardSnapshot, frame: &mut Frame, area: Rect) {
    let deploy_pct = if snap.total_capital > rust_decimal::Decimal::ZERO {
        (snap.deployed_capital / snap.total_capital * rust_decimal_macros::dec!(100)).round_dp(1)
    } else {
        rust_decimal::Decimal::ZERO
    };

    let pnl_color = if snap.daily_pnl >= rust_decimal::Decimal::ZERO {
        Color::Green
    } else {
        Color::Red
    };

    // Aggregate Q-Score and LP status
    let total_q: rust_decimal::Decimal = snap.markets.iter().map(|m| m.estimated_q).sum();
    let all_dual = snap.markets.iter().all(|m| m.reward_qualified);
    let any_active = snap.markets.iter().any(|m| m.active_orders > 0);
    let lp_label = if all_dual && !snap.markets.is_empty() {
        "DUAL"
    } else if any_active {
        "1-SIDE"
    } else {
        "NONE"
    };
    let lp_color = if all_dual && !snap.markets.is_empty() {
        Color::Green
    } else if any_active {
        Color::Yellow
    } else {
        Color::DarkGray
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(" Capital:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" ${:.0}", snap.total_capital)),
            Span::styled(
                format!("  Deployed: ${:.0} ({deploy_pct}%)", snap.deployed_capital),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(vec![
            Span::styled(" PnL:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!(" ${:.2}", snap.daily_pnl),
                Style::default().fg(pnl_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  Fills: {}", snap.fill_count)),
        ]),
        Line::from(vec![
            Span::styled(" Q-Score:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" {:.1}", total_q)),
            Span::raw("  LP: "),
            Span::styled(lp_label, Style::default().fg(lp_color).add_modifier(Modifier::BOLD)),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Strategy")),
        area,
    );
}

fn render_market_table(snap: &DashboardSnapshot, frame: &mut Frame, area: Rect) {
    let header = Row::new(vec![
        Cell::from("Market"),
        Cell::from("Mid"),
        Cell::from("Sprd"),
        Cell::from("Bid"),
        Cell::from("Ask"),
        Cell::from("IIR"),
        Cell::from("YES"),
        Cell::from("NO"),
        Cell::from("Value"),
        Cell::from("Ord"),
        Cell::from("Q"),
        Cell::from("LP"),
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

            let lp_style = if m.reward_qualified {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else if m.active_orders > 0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let lp_label = if m.reward_qualified {
                "DUAL"
            } else if m.active_orders > 0 {
                "1-SD"
            } else {
                "OFF"
            };

            Row::new(vec![
                Cell::from(truncate_str(&m.name, 18)),
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
                Cell::from(format!("{:.0}", m.yes_shares)),
                Cell::from(format!("{:.0}", m.no_shares)),
                Cell::from(format!("${:.0}", m.yes_value + m.no_value)),
                Cell::from(format!("{}", m.active_orders)),
                Cell::from(format!("{:.1}", m.estimated_q)),
                Cell::from(lp_label).style(lp_style),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Min(14),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Length(4),
            Constraint::Length(6),
            Constraint::Length(5),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Markets"));

    frame.render_widget(table, area);
}

/// Render Pricing Factors + Execution Stats panels side by side
fn render_factor_panels(snap: &DashboardSnapshot, frame: &mut Frame, area: Rect) {
    let cols = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .split(area);

    render_pricing_panel(snap, frame, cols[0]);
    render_execution_panel(snap, frame, cols[1]);
}

fn render_pricing_panel(snap: &DashboardSnapshot, frame: &mut Frame, area: Rect) {
    // Display pricing factors for the first market (or aggregate)
    if snap.markets.is_empty() {
        frame.render_widget(
            Paragraph::new(" No markets")
                .block(Block::default().borders(Borders::ALL).title("Pricing")),
            area,
        );
        return;
    }

    let mut lines = Vec::new();
    for m in &snap.markets {
        let name_short = truncate_str(&m.name, 12);
        let settle_str = match m.hours_to_settlement {
            Some(h) if h > 48.0 => format!("{:.0}d", h / 24.0),
            Some(h) if h > 0.0 => format!("{:.0}h", h),
            Some(_) => "IMMIN".to_string(),
            None => "N/A".to_string(),
        };

        let tf_color = if m.tf.is_zero() {
            Color::Red
        } else if m.tf > rust_decimal_macros::dec!(1.0) {
            Color::Yellow
        } else {
            Color::Green
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {name_short}"), Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" VAF:{:.2}", m.vaf)),
            Span::styled(format!(" TF:{:.1}", m.tf), Style::default().fg(tf_color)),
            Span::raw(format!(" Skew:{:.3}", m.skew)),
            Span::raw(format!(" Stl:{settle_str}")),
        ]));
    }

    // Pad to fill 4 lines if needed
    while lines.len() < 3 {
        lines.push(Line::from(""));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Pricing Factors")),
        area,
    );
}

fn render_execution_panel(snap: &DashboardSnapshot, frame: &mut Frame, area: Rect) {
    let ghost_color = if snap.ghost_fill_count > 0 {
        Color::Red
    } else {
        Color::Green
    };

    let cancel_rate = if snap.orders_placed_total > 0 {
        (snap.orders_cancelled_total as f64 / snap.orders_placed_total as f64 * 100.0) as u64
    } else {
        0
    };

    let ghost_rate = if snap.orders_placed_total > 0 {
        (snap.ghost_fill_count as f64 / snap.orders_placed_total as f64 * 100.0) as u64
    } else {
        0
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(" Placed: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", snap.orders_placed_total)),
            Span::styled("  Cancelled: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", snap.orders_cancelled_total)),
        ]),
        Line::from(vec![
            Span::styled(" Ghost:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{}", snap.ghost_fill_count), Style::default().fg(ghost_color)),
            Span::styled("  Cancel%: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{cancel_rate}%")),
            Span::styled("  Ghost%: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{ghost_rate}%")),
        ]),
        Line::from(vec![
            Span::styled(" Fills:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", snap.fill_count),
                Style::default().fg(Color::Cyan),
            ),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Execution")),
        area,
    );
}

fn render_footer(snap: &DashboardSnapshot, frame: &mut Frame, area: Rect) {
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
