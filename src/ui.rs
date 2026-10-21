//! Terminal rendering. The aggregator owns data; the UI owns selection only.
//!
//! Lightweight polish rules (no new dependencies):
//! * semantic colors — connection state (green/yellow/red), DIRECT chains
//!   dimmed, proxied chains accented, unresolved attribution reasons in
//!   yellow, byte magnitudes tinted from KiB→MiB→GiB;
//! * a share bar (█ blocks) proportional to each row's share of the
//!   busiest connection;
//! * the active sort column header carries a direction arrow;
//! * numeric columns are right-aligned; the selected row is bold.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row as TuiRow, Table, TableState};
use ratatui::Frame;

use crate::state::{human_bytes, Dashboard, SortMode, OFFLINE_STATUS};

pub const KEYS_HELP: &str = "s sort  q quit  arrows select";
pub const SCOPE_NOTE: &str =
    "scope: connections traversing mihomo only; direct traffic is NOT visible";

const SHARE_BAR_WIDTH: usize = 6;

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
    let warning = dashboard.warning.as_deref();
    let warn_rows: u16 = if warning.is_some() { 1 } else { 0 };
    if frame.area().height < 11 + warn_rows || frame.area().width < 70 {
        let mut text = format!(
            "mihomo-top: {}{}\nResize to at least 70x11; q quits",
            dashboard.connection_state,
            if dashboard.stale { " (stale)" } else { "" }
        );
        if let Some(warning) = warning {
            text.push('\n');
            text.push_str(warning);
        }
        frame.render_widget(Paragraph::new(text), frame.area());
        return;
    }
    let mut constraints = vec![
        Constraint::Length(6),
        Constraint::Min(3),
        Constraint::Length(2),
    ];
    if warning.is_some() {
        constraints.insert(1, Constraint::Length(1));
    }
    let sections = Layout::vertical(constraints).split(frame.area());
    draw_header(frame, sections[0], dashboard);
    let mut table_at = 1;
    if let Some(warning) = warning {
        frame.render_widget(
            Paragraph::new(safe_text(warning))
                .style(Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            sections[1],
        );
        table_at = 2;
    }
    draw_table(frame, sections[table_at], dashboard, ui);
    let detail = ui
        .table
        .selected()
        .and_then(|i| dashboard.rows.get(i))
        .and_then(|row| row.attribution_reason.as_deref())
        .unwrap_or(SCOPE_NOTE);
    frame.render_widget(
        Paragraph::new(vec![Line::from(KEYS_HELP), Line::from(safe_text(detail))]),
        sections[table_at + 1],
    );
}

/// Traffic-light color for the connection state word.
fn state_style(state: &str, stale: bool) -> Style {
    if state == "offline" {
        return Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    }
    if stale {
        return Style::new().fg(Color::Red).add_modifier(Modifier::BOLD);
    }
    match state {
        "connected" => Style::new().fg(Color::Green),
        "connecting" | "reconnecting" => Style::new().fg(Color::Yellow),
        _ => Style::new().fg(Color::Red),
    }
}

/// Chain coloring: DIRECT is a non-event (dim); anything proxied is accented.
fn chain_style(chain: &str) -> Style {
    if chain == OFFLINE_STATUS {
        Style::new().fg(Color::Yellow)
    } else if chain.eq_ignore_ascii_case("direct") || chain.is_empty() {
        Style::new().fg(Color::DarkGray)
    } else {
        Style::new().fg(Color::Cyan)
    }
}

/// Attribution reasons are diagnostics, not facts: mark them visually.
fn process_style(is_reason: bool) -> Style {
    if is_reason {
        Style::new().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

fn bytes_style(value: u64) -> Style {
    if value >= 1024 * 1024 * 1024 {
        Style::new().fg(Color::LightRed)
    } else if value >= 1024 * 1024 {
        Style::new().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

/// "█" blocks proportional to `share` (0.0..=1.0), e.g. "▓▓·····" — kept to
/// one eighth-block resolution like classic `top` bars.
fn share_bar(share: f64) -> String {
    let eighths = (share.clamp(0.0, 1.0) * SHARE_BAR_WIDTH as f64).round() as usize;
    let full = "█".repeat(eighths.min(SHARE_BAR_WIDTH));
    format!("{full:<width$}", width = SHARE_BAR_WIDTH).replace(' ', "·")
}

fn draw_header(frame: &mut Frame, area: Rect, dashboard: &Dashboard) {
    let sort = match dashboard.sort_mode {
        SortMode::BytesDesc => "bytes",
        SortMode::NewestFirst => "newest",
    };
    let title = format!("mihomo-top | {} | sort:{sort}", dashboard.controller);
    let state = if dashboard.stale && dashboard.connection_state != "offline" {
        format!("{} (STALE)", dashboard.connection_state)
    } else {
        dashboard.connection_state.clone()
    };
    let state_style = state_style(&dashboard.connection_state, dashboard.stale);
    let text = vec![
        Line::from(vec![
            Span::styled("up ", Style::new().fg(Color::DarkGray)),
            Span::styled(
                human_bytes(dashboard.upload_total),
                Style::new().fg(Color::LightGreen).bold(),
            ),
            Span::styled("  down ", Style::new().fg(Color::DarkGray)),
            Span::styled(
                human_bytes(dashboard.download_total),
                Style::new().fg(Color::LightGreen).bold(),
            ),
            Span::styled("  state: ", Style::new().fg(Color::DarkGray)),
            Span::styled(state, state_style),
        ]),
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
    let max_bytes = dashboard
        .rows
        .iter()
        .map(|row| row.upload.saturating_add(row.download))
        .max()
        .unwrap_or(1)
        .max(1);
    let rows = dashboard.rows.iter().map(|row| {
        let pane = match (&row.session, &row.pane_id) {
            (Some(session), Some(pane)) => format!("{session}/{pane}"),
            _ => String::new(),
        };
        let offline = row.proxy_status.as_deref() == Some(OFFLINE_STATUS);
        let unresolved = row.process.is_empty() && !offline;
        let process = if !row.process.is_empty() {
            row.process.as_str()
        } else if offline {
            OFFLINE_STATUS
        } else {
            row.attribution_status.as_str()
        };
        let total = row.upload.saturating_add(row.download);
        let cells = [
            Cell::from(safe_text(&row.host)),
            Cell::from(Text::from(row.destination_port.clone()).alignment(Alignment::Right)),
            Cell::from(row.network.clone()),
            Cell::from(safe_text(process)).style(if offline {
                Style::new().fg(Color::Yellow)
            } else {
                process_style(unresolved)
            }),
            Cell::from(safe_text(&pane)),
            Cell::from(safe_text(&if row.rule_payload.is_empty() {
                row.rule.clone()
            } else {
                format!("{}/{}", row.rule, row.rule_payload)
            })),
            Cell::from(safe_text(&row.chain)).style(chain_style(&row.chain)),
            Cell::from(Text::from(human_bytes(row.upload)).alignment(Alignment::Right))
                .style(bytes_style(row.upload)),
            Cell::from(Text::from(human_bytes(row.download)).alignment(Alignment::Right))
                .style(bytes_style(row.download)),
            Cell::from(
                Text::from(
                    row.age_seconds
                        .map(|age| format!("{age}s"))
                        .unwrap_or_else(|| "—".into()),
                )
                .alignment(Alignment::Right),
            ),
            Cell::from(share_bar(total as f64 / max_bytes as f64))
                .style(Style::new().fg(Color::Green)),
        ];
        TuiRow::new(cells)
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
        Constraint::Length(SHARE_BAR_WIDTH as u16),
    ];
    // active sort column gets a direction arrow
    let (up_title, age_title) = match dashboard.sort_mode {
        SortMode::BytesDesc => ("Up↓", "Age"),
        SortMode::NewestFirst => ("Up", "Age↑"),
    };
    let header = TuiRow::new([
        "Host",
        "Port",
        "Net",
        "Process",
        "Session/Pane",
        "Rule",
        "Chain(exit)",
        up_title,
        "Down",
        age_title,
        "Share",
    ])
    .style(Style::new().add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(
        Table::new(rows, widths)
            .header(header)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(if dashboard.warning.is_some() {
                        " processes "
                    } else {
                        " connections "
                    }),
            )
            .row_highlight_style(
                Style::new()
                    .bg(ratatui::style::Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
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

    fn sample_dashboard() -> Dashboard {
        Dashboard {
            controller: "127.0.0.1:9090".into(),
            connection_state: "connected".into(),
            stale: false,
            last_error: None,
            stats_snapshot: StatsSnapshot {
                ws_reconnects: 7,
                ..StatsSnapshot::default()
            },
            rows: vec![
                Row {
                    host: "example.test".into(),
                    process: "curl".into(),
                    session: Some("work".into()),
                    pane_id: Some("%1".into()),
                    chain: "Proxy→Node".into(),
                    upload: 5 * 1024 * 1024, // 5 MiB → yellow, full share bar
                    download: 512,
                    ..Row::default()
                },
                Row {
                    host: "dns.test".into(),
                    process: String::new(),
                    attribution_status: "unresolved".into(),
                    chain: "DIRECT".into(),
                    upload: 0,
                    download: 1024,
                    ..Row::default()
                },
            ],
            ..Dashboard::default()
        }
    }

    fn rendered(dashboard: &Dashboard) -> (String, Vec<Style>) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 24)).unwrap();
        terminal
            .draw(|frame| draw(frame, dashboard, &mut UiState::new()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let styles = buffer.content().iter().map(|cell| cell.style()).collect();
        (text, styles)
    }

    #[test]
    fn status_and_reconnect_counter_are_visible_inside_header() {
        let mut dashboard = sample_dashboard();
        dashboard.connection_state = "reconnecting".into();
        dashboard.stale = true;
        dashboard.last_error = Some("handshake rejected: HTTP 401".into());
        let (content, _) = rendered(&dashboard);
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
    fn offline_banner_is_yellow_and_marks_the_process() {
        let mut dashboard = sample_dashboard();
        dashboard.connection_state = "offline".into();
        dashboard.stale = true;
        dashboard.warning = Some(crate::state::offline_warning(&dashboard.controller));
        dashboard.rows[0].chain = OFFLINE_STATUS.into();
        dashboard.rows[0].proxy_status = Some(OFFLINE_STATUS.into());
        dashboard.rows[0].chains.clear();
        let (content, styles) = rendered(&dashboard);
        for value in [
            "WARNING: Mihomo offline (127.0.0.1:9090 unreachable)",
            "processes are NOT proxied",
            "mihomo-offline",
            "curl",
            "processes",
        ] {
            assert!(content.contains(value), "missing {value}");
        }
        assert!(
            styles.iter().any(|style| {
                style.fg == Some(Color::Yellow) && style.add_modifier.contains(Modifier::BOLD)
            }),
            "offline warning must be yellow"
        );
    }

    #[test]
    fn polish_elements_render() {
        let (content, _) = rendered(&sample_dashboard());
        assert!(content.contains("Up↓"), "sort arrow on bytes default");
        assert!(content.contains("Share"), "share column present");
        assert!(content.contains("██████"), "busiest row fills its bar");
        assert!(content.contains("······"), "empty rows show an empty bar");
    }

    #[test]
    fn state_word_is_color_coded() {
        let mut dashboard = sample_dashboard(); // connected => green
        let (_, styles) = rendered(&dashboard);
        assert!(
            styles.iter().any(|style| style.fg == Some(Color::Green)),
            "connected state must be green"
        );
        dashboard.connection_state = "reconnecting".into();
        let (_, styles) = rendered(&dashboard);
        assert!(
            styles.iter().any(|style| style.fg == Some(Color::Yellow)),
            "reconnecting state must be yellow"
        );
    }

    #[test]
    fn direct_chain_is_dimmed_and_proxied_accented() {
        let (_, styles) = rendered(&sample_dashboard());
        assert!(
            styles.iter().any(|style| style.fg == Some(Color::DarkGray)),
            "DIRECT chain must be dimmed"
        );
        assert!(
            styles.iter().any(|style| style.fg == Some(Color::Cyan)),
            "proxied chain must be accented"
        );
    }

    #[test]
    fn sort_arrow_follows_mode() {
        let mut dashboard = sample_dashboard();
        dashboard.sort_mode = SortMode::NewestFirst;
        let (content, _) = rendered(&dashboard);
        assert!(
            content.contains("Age↑"),
            "newest-first marks the Age column"
        );
        assert!(!content.contains("Up↓"));
    }

    #[test]
    fn share_bar_proportions() {
        assert_eq!(share_bar(1.0), "██████");
        assert_eq!(share_bar(0.0), "······");
        assert_eq!(share_bar(0.5), "███···");
        assert_eq!(share_bar(2.0), "██████"); // clamped
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
