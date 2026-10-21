//! Shared expectations for the offline report tests and the live lab.
//!
//! mihomo-top does not read `HTTP_PROXY` / `ALL_PROXY` and does not query an
//! IP-echo service. The lab report's status, IP, and connectivity columns are
//! therefore assembled from two sources: the client (did the transfer work,
//! and what fake egress address did the mock return?) and one `--once`
//! snapshot (is that transfer visible, and which destination did the
//! controller report?).

use std::net::IpAddr;

pub const FAKE_EGRESS_IP: &str = "203.0.113.10";
pub const LAB_SECRET: &str = "lab-secret-not-a-credential";
pub const SESSION_NAME: &str = "mhtlab";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    None,
    Http,
    Socks,
    HttpClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    MockProxy,
    MockDirect,
    Closed,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub scenario: &'static str,
    pub comm: &'static str,
    pub kind: ProxyKind,
    pub target: Target,
    pub connectivity: bool,
    pub egress_ip: Option<&'static str>,
    pub visible: bool,
    pub chain: Option<&'static str>,
    pub in_tmux: bool,
}

pub fn matrix_cases() -> Vec<Case> {
    vec![
        Case {
            scenario: "no-proxy",
            comm: "mht-none",
            kind: ProxyKind::None,
            target: Target::MockDirect,
            connectivity: true,
            egress_ip: Some(FAKE_EGRESS_IP),
            visible: false,
            chain: None,
            in_tmux: false,
        },
        Case {
            scenario: "http-proxy",
            comm: "mht-http",
            kind: ProxyKind::Http,
            target: Target::MockProxy,
            connectivity: true,
            egress_ip: Some(FAKE_EGRESS_IP),
            visible: true,
            chain: Some("mock-upstream"),
            in_tmux: false,
        },
        Case {
            scenario: "socks-proxy",
            comm: "mht-socks",
            kind: ProxyKind::Socks,
            target: Target::MockProxy,
            connectivity: true,
            egress_ip: Some(FAKE_EGRESS_IP),
            visible: true,
            chain: Some("mock-upstream"),
            in_tmux: true,
        },
        Case {
            scenario: "http-direct",
            comm: "mht-direct",
            kind: ProxyKind::Http,
            target: Target::MockDirect,
            connectivity: true,
            egress_ip: Some(FAKE_EGRESS_IP),
            visible: true,
            chain: Some("DIRECT"),
            in_tmux: false,
        },
        Case {
            scenario: "wrong-port",
            comm: "mht-bad",
            kind: ProxyKind::HttpClosed,
            target: Target::Closed,
            connectivity: false,
            egress_ip: None,
            visible: false,
            chain: None,
            in_tmux: false,
        },
    ]
}

#[derive(Debug, Clone)]
pub struct ClientObs {
    pub scenario: String,
    pub proxy_env: String,
    pub connectivity: bool,
    pub egress_ip: Option<String>,
    pub pid: u32,
    pub comm: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConnRow {
    pub pid: Option<u32>,
    pub process: String,
    pub chain: String,
    pub destination_ip: String,
    pub destination_port: String,
    pub attribution_status: String,
    pub session: Option<String>,
    pub pane_id: Option<String>,
    pub network: String,
}

#[derive(Debug, Clone)]
pub struct ReportRow {
    pub scenario: String,
    pub proxy_env: String,
    pub connectivity: String,
    pub egress_ip: String,
    pub reported_ip: String,
    pub status: String,
    pub passed: bool,
    pub detail: String,
}

pub struct Ports {
    pub http: u16,
    pub socks: u16,
    pub mock_proxy: u16,
    pub mock_direct: u16,
}

pub fn proxy_env_for(kind: ProxyKind, ports: &Ports) -> String {
    match kind {
        ProxyKind::None => "none".to_string(),
        ProxyKind::Http => format!("http://127.0.0.1:{}", ports.http),
        ProxyKind::Socks => format!("socks5://127.0.0.1:{}", ports.socks),
        ProxyKind::HttpClosed => "http://127.0.0.1:1".to_string(),
    }
}

pub fn destination_port(case: &Case, ports: &Ports) -> Option<u16> {
    match case.target {
        Target::MockProxy => Some(ports.mock_proxy),
        Target::MockDirect => Some(ports.mock_direct),
        Target::Closed => None,
    }
}

pub fn assess_case(case: &Case, client: &ClientObs, rows: &[ConnRow], ports: &Ports) -> ReportRow {
    let mut problems = Vec::new();
    let expected_env = proxy_env_for(case.kind, ports);
    if client.scenario != case.scenario {
        problems.push(format!("scenario {} != {}", client.scenario, case.scenario));
    }
    if client.comm != case.comm {
        problems.push(format!("comm {} != {}", client.comm, case.comm));
    }
    if client.proxy_env != expected_env {
        problems.push(format!("proxy env {} != {expected_env}", client.proxy_env));
    }
    if client.connectivity != case.connectivity {
        problems.push(format!(
            "connectivity {} != {}",
            client.connectivity, case.connectivity
        ));
    }
    if client.egress_ip.as_deref() != case.egress_ip {
        problems.push(format!(
            "egress ip {:?} != {:?}",
            client.egress_ip, case.egress_ip
        ));
    }
    if let Some(error) = &client.error {
        if case.connectivity {
            problems.push(format!("client error: {error}"));
        }
    }

    let matches: Vec<&ConnRow> = rows
        .iter()
        .filter(|row| row.pid == Some(client.pid))
        .collect();
    let reported_ip = matches
        .first()
        .map(|row| row.destination_ip.clone())
        .unwrap_or_else(|| "-".to_string());
    let status = if let Some(row) = matches.first() {
        let pane = match (&row.session, &row.pane_id) {
            (Some(session), Some(pane)) => format!(" pane={session}/{pane}"),
            _ => String::new(),
        };
        format!("visible {} {}{pane}", row.chain, row.attribution_status)
    } else {
        "not-in-mihomo".to_string()
    };

    if case.visible {
        if matches.len() != 1 {
            problems.push(format!(
                "expected one row for pid {}, found {}",
                client.pid,
                matches.len()
            ));
        }
        if let Some(row) = matches.first() {
            if row.process != case.comm {
                problems.push(format!("process {} != {}", row.process, case.comm));
            }
            if Some(row.chain.as_str()) != case.chain {
                problems.push(format!("chain {} != {:?}", row.chain, case.chain));
            }
            if row.attribution_status != "resolved" {
                problems.push(format!("attribution {}", row.attribution_status));
            }
            if row.network != "tcp" {
                problems.push(format!("network {}", row.network));
            }
            if row.destination_ip != "127.0.0.1" {
                problems.push(format!("destination ip {}", row.destination_ip));
            }
            if let Some(port) = destination_port(case, ports) {
                if row.destination_port != port.to_string() {
                    problems.push(format!(
                        "destination port {} != {port}",
                        row.destination_port
                    ));
                }
            }
            if case.in_tmux {
                if row.session.as_deref() != Some(SESSION_NAME) {
                    problems.push(format!("session {:?} != {SESSION_NAME}", row.session));
                }
                if !row
                    .pane_id
                    .as_deref()
                    .is_some_and(|pane| pane.starts_with('%'))
                {
                    problems.push(format!("pane {:?}", row.pane_id));
                }
            } else if row.session.is_some() || row.pane_id.is_some() {
                problems.push(format!(
                    "unexpected pane {:?}/{:?}",
                    row.session, row.pane_id
                ));
            }
        }
    } else if !matches.is_empty() {
        problems.push(format!("pid {} was visible and should not be", client.pid));
    }

    ReportRow {
        scenario: case.scenario.to_string(),
        proxy_env: client.proxy_env.clone(),
        connectivity: if client.connectivity { "ok" } else { "failed" }.to_string(),
        egress_ip: client.egress_ip.clone().unwrap_or_else(|| "-".to_string()),
        reported_ip,
        status,
        passed: problems.is_empty(),
        detail: problems.join("; "),
    }
}

pub fn format_report(monitor: &str, rows: &[ReportRow]) -> String {
    let mut lines = vec![
        monitor.to_string(),
        String::new(),
        format!(
            "{:<14} {:<28} {:<14} {:<16} {:<16} {}",
            "scenario", "proxy", "connectivity", "egress_ip", "reported_ip", "status"
        ),
    ];
    for row in rows {
        let failure = if row.passed {
            String::new()
        } else {
            format!("  FAIL {}", row.detail)
        };
        lines.push(format!(
            "{:<14} {:<28} {:<14} {:<16} {:<16} {}{failure}",
            row.scenario,
            row.proxy_env,
            row.connectivity,
            row.egress_ip,
            row.reported_ip,
            row.status
        ));
    }
    lines.join("\n")
}

pub struct ConfigSpec {
    pub http: u16,
    pub socks: u16,
    pub api: u16,
    pub upstream: u16,
    pub mock_proxy: u16,
}

pub fn render_mihomo_config(spec: &ConfigSpec) -> String {
    format!(
        "\
port: {http}
socks-port: {socks}
allow-lan: false
bind-address: 127.0.0.1
mode: rule
log-level: warning
ipv6: false
external-controller: 127.0.0.1:{api}
secret: {secret}
geo-auto-update: false
geodata-mode: false
profile:
  store-selected: false
  store-fake-ip: false
tun:
  enable: false
dns:
  enable: false
proxies:
  - name: mock-upstream
    type: http
    server: 127.0.0.1
    port: {upstream}
rules:
  - DST-PORT,{mock_proxy},mock-upstream
  - MATCH,DIRECT
",
        http = spec.http,
        socks = spec.socks,
        api = spec.api,
        secret = LAB_SECRET,
        upstream = spec.upstream,
        mock_proxy = spec.mock_proxy,
    )
}

pub fn config_is_local_only(config: &str) -> Result<(), String> {
    if !config.contains("tun:\n  enable: false\n") {
        return Err("tun must be explicitly disabled".into());
    }
    if config.contains("subscribe") || config.contains("proxy-providers") {
        return Err("lab config must not use a subscription".into());
    }
    if !config.contains(&format!("secret: {LAB_SECRET}")) {
        return Err("lab secret marker missing".into());
    }
    for line in config.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("http://") || lower.contains("https://") || lower.contains("socks5://") {
            return Err(format!("unexpected URL in config: {line}"));
        }
    }
    if !config.contains("external-controller: 127.0.0.1:") {
        return Err("controller is not numeric loopback".into());
    }
    Ok(())
}

/// Hide addresses that are not loopback or the lab's documentation egress IP.
pub fn redact_public_ips(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let Some(start) = rest.find(|ch: char| ch.is_ascii_hexdigit() || ch == ':' || ch == '[')
        else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let len = tail
            .find(|ch: char| !(ch.is_ascii_hexdigit() || matches!(ch, '.' | ':' | '[' | ']')))
            .unwrap_or(tail.len());
        let token = &tail[..len];
        match redacted_token(token) {
            Some(replacement) => out.push_str(&replacement),
            None => out.push_str(token),
        }
        rest = &tail[len..];
    }
    out
}

fn redacted_token(token: &str) -> Option<String> {
    let trimmed = token.trim_matches(|ch| ch == '[' || ch == ']');
    if trimmed.is_empty() {
        return None;
    }
    if trimmed
        .parse::<IpAddr>()
        .ok()
        .is_some_and(|ip| !keep_ip(&ip))
    {
        return Some("<public-ip>".to_string());
    }
    if let Some((host, port)) = trimmed.rsplit_once(':') {
        if port.chars().all(|ch| ch.is_ascii_digit()) && !port.is_empty() {
            if let Ok(ip) = host.parse::<IpAddr>() {
                if !keep_ip(&ip) {
                    return Some(format!("<public-ip>:{port}"));
                }
            }
        }
    }
    None
}

fn keep_ip(ip: &IpAddr) -> bool {
    let canonical = ip.to_canonical();
    if canonical.is_loopback() || canonical.is_unspecified() {
        return true;
    }
    match canonical {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            octets[0] == 203 && octets[1] == 0 && octets[2] == 113
        }
        IpAddr::V6(_) => false,
    }
}
