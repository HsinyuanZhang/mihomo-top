//! Terminal rendering. The aggregator owns data; the UI owns selection only.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row as TuiRow, Table, TableState};
use ratatui::Frame;

use crate::state::{human_bytes, Dashboard, SortMode};

pub const KEYS_HELP: &str = "s sort  q quit  arrows select";
pub const SCOPE_NOTE: &str =
    "scope: connections traversing mihomo only; direct traffic is NOT visible";

pub struct UiState {
    pub table: TableState,
}
impl UiState {
    pub fn new() -> Self {
        let mut table = TableState::default();
        table.select(Some(0));
        Self { table }
    }
}
impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn draw(frame: &mut Frame, dashboard: &Dashboard, ui: &mut UiState) {
    if frame.area().height < 11 || frame.area().width < 70 {
        frame.render_widget(
            Paragraph::new(format!(
                "mihomo-top: {}{}\nResize to at least 70x11; q quits",
                dashboard.connection_state,
                if dashboard.stale { " (stale)" } else { "" }
            )),
            frame.area(),
        );
        return;
    }
    let sections = Layout::vertical([
        Constraint::Length(6),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .split(frame.area());
    draw_header(frame, sections[0], dashboard);
    draw_table(frame, sections[1], dashboard, ui);
    let detail = ui
        .table
        .selected()
        .and_then(|i| dashboard.rows.get(i))
        .and_then(|row| row.attribution_reason.as_deref())
        .unwrap_or(SCOPE_NOTE);
    frame.render_widget(
        Paragraph::new(vec![Line::from(KEYS_HELP), Line::from(safe_text(detail))]),
        sections[2],
    );
}

fn draw_header(frame: &mut Frame, area: Rect, dashboard: &Dashboard) {
    let sort = match dashboard.sort_mode {
        SortMode::BytesDesc => "bytes",
        SortMode::NewestFirst => "newest",
    };
    let title = format!("mihomo-top | {} | sort:{sort}", dashboard.controller);
    let state = if dashboard.stale {
        format!("{} (STALE)", dashboard.connection_state)
    } else {
        dashboard.connection_state.clone()
    };
    let text = vec![
        Line::styled(
            format!(
                "up {}  down {}  state: {state}",
                human_bytes(dashboard.upload_total),
                human_bytes(dashboard.download_total)
            ),
            Style::new().bold(),
        ),
        Line::from(format!(
            "connections: {}  last frame: {}",
            dashboard.connection_count,
            dashboard.last_frame_at.as_deref().unwrap_or("none")
        )),
        Line::from(format!(
            "frames:{} scans:{} reconnects:{} parse-errors:{} skipped:{}",
            dashboard.stats_snapshot.frames,
            dashboard.stats_snapshot.proc_scans,
            dashboard.stats_snapshot.ws_reconnects,
            dashboard.stats_snapshot.parse_errors,
            dashboard.stats_snapshot.dropped_events
        )),
        Line::from(safe_text(
            dashboard.last_error.as_deref().unwrap_or(SCOPE_NOTE),
        )),
    ];
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn draw_table(frame: &mut Frame, area: Rect, dashboard: &Dashboard, ui: &mut UiState) {
    let rows = dashboard.rows.iter().map(|row| {
        let pane = match (&row.session, &row.pane_id) {
            (Some(session), Some(pane)) => format!("{session}/{pane}"),
            _ => String::new(),
        };
        let process = if row.process.is_empty() {
            row.attribution_status.as_str()
        } else {
            &row.process
        };
        let cells = [
            safe_text(&row.host),
            row.destination_port.clone(),
            row.network.clone(),
            safe_text(process),
            safe_text(&pane),
            safe_text(&if row.rule_payload.is_empty() {
                row.rule.clone()
            } else {
                format!("{}/{}", row.rule, row.rule_payload)
            }),
            safe_text(&row.chain),
            human_bytes(row.upload),
            human_bytes(row.download),
            row.age_seconds
                .map(|age| format!("{age}s"))
                .unwrap_or_else(|| "—".into()),
        ];
        TuiRow::new(cells.map(Cell::from))
    });
    let widths = [
        Constraint::Min(20),
        Constraint::Length(5),
        Constraint::Length(3),
        Constraint::Length(12),
        Constraint::Length(16),
        Constraint::Length(12),
        Constraint::Length(18),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(6),
    ];
    let header = TuiRow::new([
        "Host",
        "Port",
        "Net",
        "Process",
        "Session/Pane",
        "Rule",
        "Chain(exit)",
        "Up",
        "Down",
        "Age",
    ])
    .style(Style::new().add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(
        Table::new(rows, widths)
            .header(header)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" connections "),
            )
            .row_highlight_style(Style::new().bg(ratatui::style::Color::DarkGray)),
        area,
        &mut ui.table,
    );
}

fn safe_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Row, StatsSnapshot};

    #[test]
    fn status_and_reconnect_counter_are_visible_inside_header() {
        let dashboard = Dashboard {
            controller: "127.0.0.1:9090".into(),
            connection_state: "reconnecting".into(),
            stale: true,
            last_error: Some("handshake rejected: HTTP 401".into()),
            stats_snapshot: StatsSnapshot {
                ws_reconnects: 7,
                ..StatsSnapshot::default()
            },
            rows: vec![Row {
                host: "example.test".into(),
                process: "curl".into(),
                session: Some("work".into()),
                pane_id: Some("%1".into()),
                chain: "Proxy→Node".into(),
                ..Row::default()
            }],
            ..Dashboard::default()
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 24)).unwrap();
        terminal
            .draw(|frame| draw(frame, &dashboard, &mut UiState::new()))
            .unwrap();
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for value in [
            "mihomo-top",
            "reconnecting (STALE)",
            "reconnects:7",
            "HTTP 401",
            "example.test",
            "work/%1",
            "Proxy→Node",
        ] {
            assert!(content.contains(value), "missing {value}");
        }
    }

    #[test]
    fn small_terminal_keeps_state_visible() {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 5)).unwrap();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    &Dashboard {
                        connection_state: "connecting".into(),
                        stale: true,
                        ..Dashboard::default()
                    },
                    &mut UiState::new(),
                )
            })
            .unwrap();
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(content.contains("connecting (stale)"));
    }
}
