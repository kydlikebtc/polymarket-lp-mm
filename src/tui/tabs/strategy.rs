use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::tui::app::App;
use crate::tui::snapshot::DashboardSnapshot;

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    let snap = match &app.snapshot {
        Some(s) => s,
        None => {
            frame.render_widget(
                Paragraph::new("Waiting for data...")
                    .block(Block::default().borders(Borders::ALL).title("Strategy")),
                area,
            );
            return;
        }
    };

    let chunks = Layout::vertical([
        Constraint::Min(0),    // Market strategy table
        Constraint::Length(5), // Selected market detail
        Constraint::Length(3), // Footer with key hints
    ])
    .split(area);

    render_strategy_table(app, snap, frame, chunks[0]);
    render_detail_panel(app, snap, frame, chunks[1]);
    render_footer(frame, chunks[2]);
}

fn render_strategy_table(app: &App, snap: &DashboardSnapshot, frame: &mut Frame, area: Rect) {
    let header = Row::new(vec![
        Cell::from(""),
        Cell::from("Market"),
        Cell::from("Profile"),
        Cell::from("Mid"),
        Cell::from("IIR"),
        Cell::from("VAF"),
        Cell::from("TF"),
        Cell::from("Skew"),
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
        .enumerate()
        .map(|(idx, m)| {
            let is_selected = idx == app.selected_strategy_market;

            let status_cell = if m.enabled {
                Cell::from(Span::styled("ON", Style::default().fg(Color::Green)))
            } else {
                Cell::from(Span::styled("--", Style::default().fg(Color::DarkGray)))
            };

            let row_style = if !m.enabled {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };

            let profile_style = match m.profile_name.as_str() {
                "conservative" => Style::default().fg(Color::Blue),
                "aggressive" => Style::default().fg(Color::Red),
                "balanced" => Style::default().fg(Color::Green),
                _ => Style::default().fg(Color::White),
            };

            let lp_style = if !m.enabled {
                Style::default().fg(Color::DarkGray)
            } else if m.reward_qualified {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else if m.active_orders > 0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let lp_label = if !m.enabled {
                "OFF"
            } else if m.reward_qualified {
                "DUAL"
            } else if m.active_orders > 0 {
                "1-SD"
            } else {
                "OFF"
            };

            let name_display = if is_selected {
                format!(">{}", truncate_str(&m.name, 19))
            } else {
                format!(" {}", truncate_str(&m.name, 19))
            };

            let row = Row::new(vec![
                status_cell,
                Cell::from(name_display),
                Cell::from(Span::styled(
                    truncate_str(&m.profile_name, 12),
                    profile_style,
                )),
                Cell::from(format!("{:.4}", m.midpoint)),
                Cell::from(format!("{:.3}", m.iir)),
                Cell::from(format!("{:.2}", m.vaf)),
                Cell::from(format!("{:.1}", m.tf)),
                Cell::from(format!("{:.3}", m.skew)),
                Cell::from(format!("{}", m.active_orders)),
                Cell::from(format!("{:.1}", m.estimated_q)),
                Cell::from(lp_label).style(lp_style),
            ])
            .style(row_style);

            if is_selected {
                row.style(row_style.add_modifier(Modifier::REVERSED))
            } else {
                row
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),  // ON/--
            Constraint::Min(16),   // Market name
            Constraint::Length(13), // Profile
            Constraint::Length(7),  // Mid
            Constraint::Length(7),  // IIR
            Constraint::Length(6),  // VAF
            Constraint::Length(5),  // TF
            Constraint::Length(7),  // Skew
            Constraint::Length(4),  // Ord
            Constraint::Length(6),  // Q
            Constraint::Length(5),  // LP
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Strategy Management"),
    );

    frame.render_widget(table, area);
}

fn render_detail_panel(app: &App, snap: &DashboardSnapshot, frame: &mut Frame, area: Rect) {
    let Some(market) = snap.markets.get(app.selected_strategy_market) else {
        frame.render_widget(
            Paragraph::new(" No market selected")
                .block(Block::default().borders(Borders::ALL).title("Details")),
            area,
        );
        return;
    };

    let settle_str = match market.hours_to_settlement {
        Some(h) if h > 48.0 => format!("{:.0}d", h / 24.0),
        Some(h) if h > 0.0 => format!("{:.0}h", h),
        Some(_) => "IMMINENT".to_string(),
        None => "N/A".to_string(),
    };

    let enabled_span = if market.enabled {
        Span::styled("ENABLED", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
    } else {
        Span::styled("DISABLED", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(" Market: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&market.name),
            Span::raw("  "),
            enabled_span,
            Span::raw(format!("  Profile: ")),
            Span::styled(&market.profile_name, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled(" Settlement: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&settle_str),
            Span::raw(format!(
                "  YES: {:.0}  NO: {:.0}  Value: ${:.0}",
                market.yes_shares,
                market.no_shares,
                market.yes_value + market.no_value
            )),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Selected Market"),
        ),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect) {
    let footer = Line::from(vec![
        Span::styled(
            " [j/k] Select  [e] Toggle  [Enter] Edit  [p] Profile  [a] Add  [d] Delete  [r] Reset  [q] Quit ",
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    frame.render_widget(
        Paragraph::new(footer).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}
