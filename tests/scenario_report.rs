mod common;

use common::{
    assess_case, config_is_local_only, format_report, matrix_cases, redact_public_ips,
    render_mihomo_config, ClientObs, ConfigSpec, ConnRow, Ports, FAKE_EGRESS_IP, SESSION_NAME,
};

fn ports() -> Ports {
    Ports {
        http: 17890,
        socks: 17891,
        mock_proxy: 43157,
        mock_direct: 43158,
    }
}

fn happy_client(case: &common::Case, proxy_env: &str, pid: u32) -> ClientObs {
    ClientObs {
        scenario: case.scenario.to_string(),
        proxy_env: proxy_env.to_string(),
        connectivity: case.connectivity,
        egress_ip: case.egress_ip.map(str::to_string),
        pid,
        comm: case.comm.to_string(),
        error: if case.connectivity {
            None
        } else {
            Some("connection refused".into())
        },
    }
}

fn row_for(case: &common::Case, pid: u32, port: u16) -> ConnRow {
    ConnRow {
        pid: Some(pid),
        process: case.comm.to_string(),
        chain: case.chain.unwrap_or("DIRECT").to_string(),
        destination_ip: "127.0.0.1".into(),
        destination_port: port.to_string(),
        attribution_status: "resolved".into(),
        session: case.in_tmux.then(|| SESSION_NAME.to_string()),
        pane_id: case.in_tmux.then(|| "%3".to_string()),
        network: "tcp".into(),
    }
}

fn happy_world() -> (Vec<common::Case>, Vec<ClientObs>, Vec<ConnRow>, Ports) {
    let spec = ports();
    let cases = matrix_cases();
    let mut clients = Vec::new();
    let mut rows = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let pid = 4000 + index as u32;
        let env = common::proxy_env_for(case.kind, &spec);
        clients.push(happy_client(case, &env, pid));
        if case.visible {
            let port = common::destination_port(case, &spec).unwrap();
            rows.push(row_for(case, pid, port));
        }
    }
    (cases, clients, rows, spec)
}

#[test]
fn matrix_report_passes_when_visibility_ip_and_connectivity_match() {
    let (cases, clients, rows, spec) = happy_world();
    let report: Vec<_> = cases
        .iter()
        .zip(clients.iter())
        .map(|(case, client)| assess_case(case, client, &rows, &spec))
        .collect();
    let text = format_report("monitor status: connected", &report);
    assert!(report.iter().all(|row| row.passed), "{text}");
    assert!(text.contains("no-proxy"));
    assert!(text.contains("not-in-mihomo"));
    assert!(text.contains("mock-upstream"));
    assert!(text.contains("DIRECT"));
    assert!(text.contains(FAKE_EGRESS_IP));
    assert!(text.contains("mhtlab/%3") || text.contains("pane=mhtlab/%3"));
}

#[test]
fn missing_proxied_row_fails_the_status_column() {
    let (cases, clients, _rows, spec) = happy_world();
    let case = cases
        .iter()
        .find(|case| case.scenario == "http-proxy")
        .unwrap();
    let client = clients
        .iter()
        .find(|client| client.scenario == "http-proxy")
        .unwrap();
    let row = assess_case(case, client, &[], &spec);
    assert!(!row.passed);
    assert_eq!(row.status, "not-in-mihomo");
    assert!(row.detail.contains("expected one row"));
}

#[test]
fn bypass_traffic_must_stay_out_of_the_snapshot() {
    let (cases, clients, mut rows, spec) = happy_world();
    let case = cases
        .iter()
        .find(|case| case.scenario == "no-proxy")
        .unwrap();
    let client = clients
        .iter()
        .find(|client| client.scenario == "no-proxy")
        .unwrap();
    rows.push(row_for(case, client.pid, spec.mock_direct));
    let row = assess_case(case, client, &rows, &spec);
    assert!(!row.passed, "{}", row.detail);
    assert!(row.detail.contains("should not be"));
}

#[test]
fn wrong_egress_ip_fails_even_when_the_connection_is_visible() {
    let (cases, mut clients, rows, spec) = happy_world();
    let case = cases
        .iter()
        .find(|case| case.scenario == "socks-proxy")
        .unwrap();
    let client = clients
        .iter_mut()
        .find(|client| client.scenario == "socks-proxy")
        .unwrap();
    client.egress_ip = Some("198.51.100.8".into());
    let row = assess_case(case, client, &rows, &spec);
    assert!(!row.passed);
    assert!(row.detail.contains("egress ip"));
}

#[test]
fn unresolved_attribution_is_not_a_successful_status() {
    let (cases, clients, mut rows, spec) = happy_world();
    let case = cases
        .iter()
        .find(|case| case.scenario == "http-direct")
        .unwrap();
    let client = clients
        .iter()
        .find(|client| client.scenario == "http-direct")
        .unwrap();
    rows.iter_mut()
        .find(|row| row.pid == Some(client.pid))
        .unwrap()
        .attribution_status = "unknown".into();
    let row = assess_case(case, client, &rows, &spec);
    assert!(!row.passed);
    assert!(row.detail.contains("attribution unknown"));
}

#[test]
fn socks_client_without_its_tmux_pane_fails() {
    let (cases, clients, mut rows, spec) = happy_world();
    let case = cases
        .iter()
        .find(|case| case.scenario == "socks-proxy")
        .unwrap();
    let client = clients
        .iter()
        .find(|client| client.scenario == "socks-proxy")
        .unwrap();
    let found = rows
        .iter_mut()
        .find(|row| row.pid == Some(client.pid))
        .unwrap();
    found.session = None;
    found.pane_id = None;
    let row = assess_case(case, client, &rows, &spec);
    assert!(!row.passed);
    assert!(row.detail.contains("session"));
}

#[test]
fn lab_config_stays_on_loopback_without_a_subscription_or_tun() {
    let config = render_mihomo_config(&ConfigSpec {
        http: 1,
        socks: 2,
        api: 3,
        upstream: 4,
        mock_proxy: 5,
    });
    config_is_local_only(&config).unwrap();
    assert!(config.contains("DST-PORT,5,mock-upstream"));
    assert!(config.contains("MATCH,DIRECT"));
    assert!(!config.contains("http://"), "{config}");
    assert!(!config.contains("https://"), "{config}");
}

#[test]
fn redaction_keeps_the_lab_addresses_and_hides_other_v4() {
    let text = "loop 127.0.0.1:19090 fake 203.0.113.10 live 203.0.113.10 and 198.51.100.20 port 203.0.113.10:80 public 104.20.23.154 time 2026-10-21T11:27:58.40773861Z v6 ::1";
    let redacted = redact_public_ips(text);
    assert!(redacted.contains("127.0.0.1:19090"), "{redacted}");
    assert!(redacted.contains("203.0.113.10"), "{redacted}");
    assert!(redacted.contains("::1"), "{redacted}");
    assert!(redacted.contains("11:27:58.40773861Z"), "{redacted}");
    assert!(!redacted.contains("198.51.100.20"), "{redacted}");
    assert!(!redacted.contains("104.20.23.154"), "{redacted}");
    assert!(redacted.contains("<public-ip>"), "{redacted}");
}
