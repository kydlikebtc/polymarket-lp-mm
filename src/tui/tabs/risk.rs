use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};

use crate::risk::RiskLevel;
use crate::tui::app::App;

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(5), // Risk level banner
        Constraint::Length(5), // IIR gauges
        Constraint::Min(0),   // Details
        Constraint::Length(3), // Status bar
    ])
    .split(area);

    render_risk_banner(app, frame, chunks[0]);
    render_iir_gauges(app, frame, chunks[1]);
    render_details(app, frame, chunks[2]);
    render_status_bar(app, frame, chunks[3]);
}

fn render_risk_banner(app: &App, frame: &mut Frame, area: Rect) {
    let snap = match &app.snapshot {
        Some(s) => s,
        None => {
            frame.render_widget(
                Paragraph::new("Waiting...")
                    .block(Block::default().borders(Borders::ALL).title("Risk Level")),
                area,
            );
            return;
        }
    };

    let (label, color) = match snap.risk_level {
        RiskLevel::L1Normal => ("L1 - NORMAL", Color::Green),
        RiskLevel::L2Warning => ("L2 - WARNING", Color::Yellow),
        RiskLevel::L3Emergency => ("L3 - EMERGENCY", Color::Red),
    };

    let duration_str = if let Some(entered_at) = snap.l2_entered_at {
        let secs = (chrono::Utc::now() - entered_at).num_seconds().max(0);
        format!(" ({}m {}s)", secs / 60, secs % 60)
    } else {
        String::new()
    };

    let text = Line::from(vec![
        Span::styled(
            format!("  {label}{duration_str}  "),
            Style::default()
                .fg(color)
                .add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK),
        ),
    ]);

    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("Risk Level")),
        area,
    );
}

fn render_iir_gauges(app: &App, frame: &mut Frame, area: Rect) {
    let snap = match &app.snapshot {
        Some(s) => s,
        None => return,
    };

    if snap.markets.is_empty() {
        return;
    }

    // Split area horizontally for each market's IIR gauge
    let constraints: Vec<Constraint> = snap
        .markets
        .iter()
        .map(|_| Constraint::Ratio(1, snap.markets.len() as u32))
        .collect();
    let gauge_areas = Layout::horizontal(constraints).split(area);

    for (i, market) in snap.markets.iter().enumerate() {
        if i >= gauge_areas.len() {
            break;
        }

        // IIR is [-1, 1], map to [0, 100] for gauge
        let iir_f64: f64 = market.iir.to_string().parse().unwrap_or(0.0);
        let ratio = ((iir_f64 + 1.0) / 2.0).clamp(0.0, 1.0);

        let gauge_color = if iir_f64.abs() >= 0.5 {
            Color::Red
        } else if iir_f64.abs() >= 0.3 {
            Color::Yellow
        } else {
            Color::Green
        };

        let gauge = Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!("{} IIR", truncate_str(&market.name, 12))),
            )
            .gauge_style(Style::default().fg(gauge_color))
            .ratio(ratio)
            .label(format!("{:.3}", market.iir));

        frame.render_widget(gauge, gauge_areas[i]);
    }
}

fn render_details(app: &App, frame: &mut Frame, area: Rect) {
    let snap = match &app.snapshot {
        Some(s) => s,
        None => return,
    };

    let chunks = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .split(area);

    // Left: Ghost fills & WS status
    let mut left_lines = vec![
        Line::from(vec![
            Span::raw("  Ghost Fills: "),
            Span::styled(
                format!("{}", snap.ghost_fill_count),
                if snap.ghost_fill_count > 0 {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(Color::Green)
                },
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::raw("  Market WS: "),
            ws_status_span(snap.market_ws_connected),
        ]),
        Line::from(vec![
            Span::raw("  User WS:   "),
            ws_status_span(snap.user_ws_connected),
        ]),
    ];
    if snap.ws_disconnect_secs > 0 {
        left_lines.push(Line::from(vec![
            Span::raw("  Disconnect: "),
            Span::styled(
                format!("{}s", snap.ws_disconnect_secs),
                Style::default().fg(Color::Red),
            ),
        ]));
    }

    frame.render_widget(
        Paragraph::new(left_lines)
            .block(Block::default().borders(Borders::ALL).title("System")),
        chunks[0],
    );

    // Right: PnL & position summary
    let pnl_color = if snap.daily_pnl >= rust_decimal_macros::dec!(0) {
        Color::Green
    } else {
        Color::Red
    };

    let total_value: rust_decimal::Decimal = snap
        .markets
        .iter()
        .map(|m| m.yes_value + m.no_value)
        .sum();

    let right_lines = vec![
        Line::from(vec![
            Span::raw("  Daily PnL:  "),
            Span::styled(
                format!("${:.2}", snap.daily_pnl),
                Style::default().fg(pnl_color).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::raw("  Fills:      "),
            Span::raw(format!("{}", snap.fill_count)),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::raw("  Total Value: "),
            Span::raw(format!("${:.2}", total_value)),
        ]),
        Line::from(vec![
            Span::raw("  Markets:     "),
            Span::raw(format!("{}", snap.markets.len())),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(right_lines)
            .block(Block::default().borders(Borders::ALL).title("Portfolio")),
        chunks[1],
    );
}

fn render_status_bar(_app: &App, frame: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::raw(" [r] Recover L3  [1-4] Tab  [q] Quit"),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn ws_status_span(connected: bool) -> Span<'static> {
    if connected {
        Span::styled("Connected", Style::default().fg(Color::Green))
    } else {
        Span::styled("Disconnected", Style::default().fg(Color::Red))
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}
