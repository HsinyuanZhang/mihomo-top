//! Live lab: real mihomo, a local mock upstream, and processes with different
//! proxy environments. Ignored unless `scripts/test-env.sh` runs it.

mod common;

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use common::{
    assess_case, config_is_local_only, destination_port, format_report, matrix_cases,
    proxy_env_for, redact_public_ips, render_mihomo_config, ClientObs, ConfigSpec, ConnRow, Ports,
    Target, LAB_SECRET, SESSION_NAME,
};

const PROXY_KEYS: [&str; 6] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];

static LABS: AtomicU64 = AtomicU64::new(0);

struct Lab {
    dir: PathBuf,
    mihomo: Option<Child>,
    server: Option<Child>,
    clients: Vec<Child>,
    tmux_socket: Option<PathBuf>,
}

impl Drop for Lab {
    fn drop(&mut self) {
        if let Some(socket) = &self.tmux_socket {
            let _ = Command::new("tmux")
                .args(["-S", &socket.to_string_lossy(), "kill-server"])
                .stderr(Stdio::null())
                .status();
        }
        for child in &mut self.clients {
            let _ = child.kill();
            let _ = child.wait();
        }
        for child in [&mut self.mihomo, &mut self.server].into_iter().flatten() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
#[ignore = "needs Linux, python3, tmux, and a mihomo binary; run scripts/test-env.sh"]
fn proxy_matrix_reports_status_ip_and_connectivity() {
    if let Err(error) = proxy_matrix() {
        panic!("{error}");
    }
}

#[test]
#[ignore = "needs Linux, python3, and a mihomo binary; run scripts/test-env.sh"]
fn stopped_controller_is_an_error_and_the_client_fails() {
    if let Err(error) = stopped_controller() {
        panic!("{error}");
    }
}

#[test]
#[ignore = "optional; set MIHOMO_TOP_LIVE_NETWORK=1 to dial example.com through mihomo"]
fn optional_live_network_egress() {
    if std::env::var("MIHOMO_TOP_LIVE_NETWORK").ok().as_deref() != Some("1") {
        eprintln!("skipping live network probe; set MIHOMO_TOP_LIVE_NETWORK=1");
        return;
    }
    if let Err(error) = live_network() {
        panic!("{}", redact_public_ips(&error));
    }
}

fn proxy_matrix() -> Result<(), String> {
    let mut lab = Lab::new("matrix")?;
    let endpoints = start_mock_server(&mut lab)?;
    let ports = listen_ports()?;
    let spec = ConfigSpec {
        http: ports.http,
        socks: ports.socks,
        api: ports.api,
        upstream: endpoints.upstream,
        mock_proxy: endpoints.mock_proxy,
    };
    let report_ports = Ports {
        http: ports.http,
        socks: ports.socks,
        mock_proxy: endpoints.mock_proxy,
        mock_direct: endpoints.mock_direct,
    };
    let config = render_mihomo_config(&spec);
    config_is_local_only(&config)?;
    fs::write(lab.dir.join("config.yaml"), &config).map_err(|e| e.to_string())?;
    write_secret(&lab.dir.join("secret"))?;
    start_mihomo(&mut lab)?;
    wait_version(ports.api)?;

    lab.tmux_socket = Some(lab.dir.join("tmux.sock"));
    let cases = matrix_cases();
    let mut spawned = Vec::new();
    for case in &cases {
        spawned.push(spawn_case(&mut lab, case, &report_ports)?);
    }
    for client in &spawned {
        if client.holds {
            wait_ready(&client.ready)?;
        }
    }
    for client in &spawned {
        if client.holds {
            assert_process_env(&client.case_name, client.pid()?, &report_ports)?;
        }
    }

    let visible: Vec<u32> = spawned
        .iter()
        .filter(|client| client.visible)
        .map(|client| client.pid())
        .collect::<Result<Vec<_>, _>>()?;
    let (snapshot, stdout, stderr) = snapshot_until(&lab, ports.api, &visible)?;
    assert_monitor(&snapshot, ports.api, true)?;
    if stdout.contains(LAB_SECRET) || stderr.contains(LAB_SECRET) {
        return Err("controller secret leaked into mihomo-top output".into());
    }

    let plain = once(&lab, ports.api, &["--no-attribution"])?;
    let disabled: Value =
        serde_json::from_str(&plain.0).map_err(|e| format!("{e}: {}", plain.0))?;
    if disabled["local_attribution"] != json!(false) {
        return Err(format!(
            "local_attribution {}",
            disabled["local_attribution"]
        ));
    }
    let disabled_rows = conn_rows(&disabled)?;
    if disabled_rows
        .iter()
        .any(|row| row.pid.is_some() || !row.process.is_empty())
    {
        return Err("disabled attribution still named a process".into());
    }
    if disabled_rows
        .iter()
        .any(|row| row.attribution_status != "disabled")
    {
        return Err("expected attribution_status disabled".into());
    }
    let proxy_port = report_ports.mock_proxy.to_string();
    let direct_port = report_ports.mock_direct.to_string();
    let proxied = disabled_rows
        .iter()
        .filter(|row| row.destination_port == proxy_port && row.chain == "mock-upstream")
        .count();
    let direct = disabled_rows
        .iter()
        .filter(|row| row.destination_port == direct_port && row.chain == "DIRECT")
        .count();
    if proxied < 2 || direct < 1 {
        return Err(format!(
            "--no-attribution lost rows: proxied={proxied} direct={direct}"
        ));
    }

    let tui = capture_tui(&lab, ports.api)?;
    if !tui.contains("mht-http") || !tui.contains("mock-upstream") || !tui.contains("connected") {
        return Err(format!("TUI capture did not show the held clients:\n{tui}"));
    }

    http_get(endpoints.mock_direct, "/release", None)?;
    let mut observations = Vec::new();
    for client in &spawned {
        observations.push(wait_result(&client.result)?);
    }
    let rows = conn_rows(&snapshot)?;
    let mut report = Vec::new();
    for case in &cases {
        let client = observations
            .iter()
            .find(|client| client.scenario == case.scenario)
            .ok_or_else(|| format!("missing client result {}", case.scenario))?;
        report.push(assess_case(case, client, &rows, &report_ports));
    }
    let tun = tun_probe();
    let monitor = format!(
        "monitor status: {}  stale={}  secret_in_use={}  controller=127.0.0.1:{}\n{}",
        snapshot["connection_state"].as_str().unwrap_or("?"),
        snapshot["stale"],
        snapshot["secret_in_use"],
        ports.api,
        tun
    );
    let text = format_report(&monitor, &report);
    let transcript = format!("{text}\n\n--- mihomo-top --once ---\n{stdout}\n--- stderr ---\n{stderr}\n--- tui ---\n{tui}\n");
    save_artifact("mihomo-top-report.txt", &text);
    save_artifact("mihomo-top-once.json", &stdout);
    save_artifact("mihomo-top-tui.txt", &tui);
    save_artifact("mihomo-top-live.txt", &transcript);
    eprintln!("{text}");
    if report.iter().any(|row| !row.passed) {
        return Err(text);
    }
    Ok(())
}

fn stopped_controller() -> Result<(), String> {
    let mut lab = Lab::new("stopped")?;
    let ports = listen_ports()?;
    // The upstream and rule ports are unused: this phase has no traffic while
    // the controller is up, then nothing is listening after mihomo exits.
    let config = render_mihomo_config(&ConfigSpec {
        http: ports.http,
        socks: ports.socks,
        api: ports.api,
        upstream: ports.http,
        mock_proxy: 9,
    });
    config_is_local_only(&config)?;
    fs::write(lab.dir.join("config.yaml"), &config).map_err(|e| e.to_string())?;
    write_secret(&lab.dir.join("secret"))?;
    start_mihomo(&mut lab)?;
    wait_version(ports.api)?;
    let (stdout, stderr) = once(&lab, ports.api, &[])?;
    let idle: Value = serde_json::from_str(&stdout).map_err(|e| format!("{e}\n{stderr}"))?;
    assert_monitor(&idle, ports.api, true)?;
    if idle["connection_count"] != 0 {
        return Err(format!(
            "idle controller reported {}",
            idle["connection_count"]
        ));
    }

    if let Some(child) = lab.mihomo.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    lab.mihomo = None;
    thread::sleep(Duration::from_millis(100));

    let output = mihomo_output(&lab, ports.api, &["--timeout-secs", "2"])?;
    if output.status.success() {
        return Err("mihomo-top succeeded against a stopped controller".into());
    }
    let err = String::from_utf8_lossy(&output.stderr);
    let out = String::from_utf8_lossy(&output.stdout);
    if !out.is_empty() {
        return Err(format!("stopped controller wrote stdout: {out}"));
    }
    if !err.contains("connection refused") || !err.contains("no valid frame") {
        return Err(format!("unexpected stopped-controller error:\n{err}"));
    }
    if err.contains(LAB_SECRET) {
        return Err("secret leaked in the controller-down error".into());
    }

    let result_path = lab.dir.join("down.json");
    let config_path = lab.dir.join("down-client.json");
    fs::write(
        &config_path,
        serde_json::to_string(&json!({
            "scenario": "controller-down",
            "comm": "mht-down",
            "target_host": "127.0.0.1",
            "target_port": 9,
            "result_file": result_path,
        }))
        .unwrap(),
    )
    .map_err(|e| e.to_string())?;
    let mut cmd = Command::new("python3");
    cmd.arg(lab_script())
        .arg("client")
        .arg("--config")
        .arg(&config_path);
    scrub(&mut cmd);
    cmd.env("HTTP_PROXY", format!("http://127.0.0.1:{}", ports.http));
    cmd.env("HTTPS_PROXY", format!("http://127.0.0.1:{}", ports.http));
    let output = cmd.output().map_err(|e| e.to_string())?;
    if !output.status.success() && output.stdout.is_empty() {
        return Err(format!(
            "down client crashed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let client: Value = serde_json::from_slice(&output.stdout).map_err(|e| {
        format!(
            "{e}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    if client["connectivity"] != false {
        return Err(format!("client reached a stopped proxy: {client}"));
    }
    let summary = format!(
        "scenario       proxy                         connectivity  egress_ip  reported_ip  status\n\
         controller-down http://127.0.0.1:{:<5}        failed        -          -            connection refused, no snapshot\n",
        ports.http
    );
    save_artifact("mihomo-top-stopped.txt", &format!("{summary}\n{err}"));
    eprintln!("{summary}");
    Ok(())
}

fn live_network() -> Result<(), String> {
    let mut lab = Lab::new("live")?;
    let ports = listen_ports()?;
    let config = render_mihomo_config(&ConfigSpec {
        http: ports.http,
        socks: ports.socks,
        api: ports.api,
        upstream: ports.http,
        mock_proxy: 9,
    });
    fs::write(lab.dir.join("config.yaml"), &config).map_err(|e| e.to_string())?;
    write_secret(&lab.dir.join("secret"))?;
    start_mihomo(&mut lab)?;
    wait_version(ports.api)?;

    let dest = example_ipv4()?;
    let ready = lab.dir.join("live.ready");
    let result = lab.dir.join("live.json");
    let release = lab.dir.join("live.release");
    fs::write(
        lab.dir.join("live-client.json"),
        serde_json::to_string(&json!({
            "scenario": "live-hold",
            "comm": "mht-live",
            "target_host": dest.to_string(),
            "target_port": 80,
            "ready_file": ready,
            "result_file": result,
            "release_file": release,
        }))
        .unwrap(),
    )
    .map_err(|e| e.to_string())?;
    let mut cmd = Command::new("python3");
    cmd.arg(lab_script())
        .arg("client")
        .arg("--config")
        .arg(lab.dir.join("live-client.json"));
    scrub(&mut cmd);
    cmd.env("ALL_PROXY", format!("socks5://127.0.0.1:{}", ports.socks));
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut held = cmd.spawn().map_err(|e| e.to_string())?;
    wait_ready(&ready)?;
    let pid = fs::read_to_string(&ready)
        .map_err(|e| e.to_string())?
        .trim()
        .parse::<u32>()
        .map_err(|e| e.to_string())?;
    let (snapshot, stdout, _stderr) = snapshot_until(&lab, ports.api, &[pid])?;
    let rows = conn_rows(&snapshot)?;
    let row = rows
        .iter()
        .find(|row| row.pid == Some(pid))
        .ok_or("live connection missing")?;
    if row.chain != "DIRECT" || row.destination_port != "80" || row.attribution_status != "resolved"
    {
        return Err(format!(
            "live row status {} {} {}",
            row.chain, row.destination_port, row.attribution_status
        ));
    }
    if row.destination_ip != dest.to_string() {
        return Err("live destination did not match the resolved example.com address".into());
    }
    if row.process != "mht-live" {
        return Err(format!("live process {}", row.process));
    }

    let echo = Command::new("python3");
    let echo_cfg = lab.dir.join("echo-client.json");
    let echo_out = lab.dir.join("echo.json");
    fs::write(
        &echo_cfg,
        serde_json::to_string(&json!({
            "scenario": "live-echo",
            "comm": "mht-echo",
            "result_file": echo_out,
        }))
        .unwrap(),
    )
    .map_err(|e| e.to_string())?;
    let mut echo = echo;
    echo.arg(lab_script())
        .arg("client")
        .arg("--config")
        .arg(&echo_cfg);
    scrub(&mut echo);
    let proxy = format!("http://127.0.0.1:{}", ports.http);
    echo.env("HTTP_PROXY", &proxy);
    echo.env("HTTPS_PROXY", &proxy);
    let echo_run = echo.output().map_err(|e| e.to_string())?;
    fs::write(&release, b"1").map_err(|e| e.to_string())?;
    let _ = held.wait();
    let echo_json: Value = serde_json::from_slice(&echo_run.stdout)
        .map_err(|e| format!("{e} {}", String::from_utf8_lossy(&echo_run.stderr)))?;
    if echo_json["connectivity"] != true {
        return Err(format!(
            "IP echo failed: {}",
            redact_public_ips(&echo_json.to_string())
        ));
    }
    let ip = echo_json["egress_ip"].as_str().unwrap_or("");
    let parsed: Ipv4Addr = ip
        .parse()
        .map_err(|_| "echo body is not an IPv4 address".to_string())?;
    if parsed.is_loopback()
        || parsed.is_private()
        || parsed.is_link_local()
        || ip == common::FAKE_EGRESS_IP
    {
        return Err("echo returned a non-public address".into());
    }
    let summary = format!(
        "monitor status: {}\nscenario      proxy                         connectivity  egress_ip     reported_ip   status\nlive-hold     socks5://127.0.0.1:{:<5}       ok            -             <public-ip>   visible DIRECT resolved\nlive-echo     http://127.0.0.1:{:<5}         ok            <public-ip>   -             direct IP echo\n",
        snapshot["connection_state"].as_str().unwrap_or("?"),
        ports.socks,
        ports.http
    );
    save_artifact(
        "mihomo-top-live-network.txt",
        &format!("{summary}\n{}", redact_public_ips(&stdout)),
    );
    eprintln!("{summary}");
    Ok(())
}

struct ListenPorts {
    http: u16,
    socks: u16,
    api: u16,
}

struct Endpoints {
    mock_proxy: u16,
    mock_direct: u16,
    upstream: u16,
}

struct Spawned {
    case_name: String,
    holds: bool,
    visible: bool,
    ready: PathBuf,
    result: PathBuf,
}

impl Lab {
    fn new(name: &str) -> Result<Self, String> {
        let n = LABS.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("mihomo-top-lab-{}-{name}-{n}", std::process::id()));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(Self {
            dir,
            mihomo: None,
            server: None,
            clients: Vec::new(),
            tmux_socket: None,
        })
    }
}

fn lab_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lab.py")
}

fn mihomo_program() -> Result<PathBuf, String> {
    let path = std::env::var("MIHOMO_BIN")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".cache/mihomo/v1.19.32/mihomo")
        });
    if !path.is_file() {
        return Err(format!(
            "mihomo binary missing at {}. Run scripts/test-env.sh",
            path.display()
        ));
    }
    Ok(path)
}

fn listen_ports() -> Result<ListenPorts, String> {
    Ok(ListenPorts {
        http: free_port()?,
        socks: free_port()?,
        api: free_port()?,
    })
}

fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    Ok(listener.local_addr().map_err(|e| e.to_string())?.port())
}

fn write_secret(path: &Path) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(LAB_SECRET.as_bytes())
        .map_err(|e| e.to_string())?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn start_mock_server(lab: &mut Lab) -> Result<Endpoints, String> {
    let endpoints = lab.dir.join("endpoints.json");
    let log = File::create(lab.dir.join("lab.log")).map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let child = Command::new("python3")
        .arg(lab_script())
        .arg("serve")
        .arg("--endpoints-file")
        .arg(&endpoints)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .spawn()
        .map_err(|e| format!("start lab server: {e}"))?;
    lab.server = Some(child);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(&endpoints) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                return Ok(Endpoints {
                    mock_proxy: value["mock_proxy_port"].as_u64().ok_or("proxy port")? as u16,
                    mock_direct: value["mock_direct_port"].as_u64().ok_or("direct port")? as u16,
                    upstream: value["upstream_port"].as_u64().ok_or("upstream port")? as u16,
                });
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "mock server did not start:\n{}",
                tail(&lab.dir.join("lab.log"))
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn start_mihomo(lab: &mut Lab) -> Result<(), String> {
    let log = File::create(lab.dir.join("mihomo.log")).map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let child = Command::new(mihomo_program()?)
        .arg("-d")
        .arg(&lab.dir)
        .arg("-f")
        .arg(lab.dir.join("config.yaml"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .spawn()
        .map_err(|e| format!("start mihomo: {e}"))?;
    lab.mihomo = Some(child);
    Ok(())
}

fn wait_version(api: u16) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = String::new();
    while Instant::now() < deadline {
        match http_get(api, "/version", Some(LAB_SECRET)) {
            Ok((200, body)) if body.contains("meta") => return Ok(()),
            Ok((code, body)) => last = format!("HTTP {code} {body}"),
            Err(error) => last = error,
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(format!("controller /version not ready: {last}"))
}

fn spawn_case(lab: &mut Lab, case: &common::Case, ports: &Ports) -> Result<Spawned, String> {
    let ready = lab.dir.join(format!("{}.ready", case.scenario));
    let result = lab.dir.join(format!("{}.json", case.scenario));
    let config_path = lab.dir.join(format!("{}.cfg.json", case.scenario));
    let target_port = destination_port(case, ports).unwrap_or(9);
    fs::write(
        &config_path,
        serde_json::to_string(&json!({
            "scenario": case.scenario,
            "comm": case.comm,
            "target_host": "127.0.0.1",
            "target_port": target_port,
            "ready_file": ready,
            "result_file": result,
        }))
        .unwrap(),
    )
    .map_err(|e| e.to_string())?;
    if case.in_tmux {
        let socket = lab.dir.join("tmux.sock");
        let proxy = proxy_env_for(case.kind, ports);
        let status = Command::new("tmux")
            .args([
                "-S",
                &socket.to_string_lossy(),
                "new-session",
                "-d",
                "-s",
                SESSION_NAME,
                "-n",
                "socks",
                "--",
                "env",
                "-u",
                "HTTP_PROXY",
                "-u",
                "http_proxy",
                "-u",
                "HTTPS_PROXY",
                "-u",
                "https_proxy",
                &format!("ALL_PROXY={proxy}"),
                "python3",
            ])
            .arg(lab_script())
            .arg("client")
            .arg("--config")
            .arg(&config_path)
            .env("TERM", "xterm-256color")
            .status()
            .map_err(|e| format!("tmux: {e}"))?;
        if !status.success() {
            return Err(format!("tmux new-session exited {status}"));
        }
        // The socket is recorded by the caller via the path convention.
        let _ = socket;
    } else {
        let mut cmd = Command::new("python3");
        cmd.arg(lab_script())
            .arg("client")
            .arg("--config")
            .arg(&config_path);
        scrub(&mut cmd);
        apply_env(&mut cmd, case, ports);
        let err = File::create(lab.dir.join(format!("{}.err", case.scenario)))
            .map_err(|e| e.to_string())?;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(err));
        lab.clients.push(
            cmd.spawn()
                .map_err(|e| format!("spawn {}: {e}", case.scenario))?,
        );
    }
    Ok(Spawned {
        case_name: case.scenario.to_string(),
        holds: !matches!(case.target, Target::Closed),
        visible: case.visible,
        ready,
        result,
    })
}

impl Spawned {
    fn pid(&self) -> Result<u32, String> {
        fs::read_to_string(&self.ready)
            .map_err(|e| format!("ready {}: {e}", self.ready.display()))?
            .trim()
            .parse()
            .map_err(|e| format!("pid: {e}"))
    }
}

fn apply_env(cmd: &mut Command, case: &common::Case, ports: &Ports) {
    match case.kind {
        common::ProxyKind::None => {}
        common::ProxyKind::Http | common::ProxyKind::HttpClosed => {
            let value = proxy_env_for(case.kind, ports);
            cmd.env("HTTP_PROXY", &value);
            cmd.env("HTTPS_PROXY", &value);
        }
        common::ProxyKind::Socks => {
            cmd.env("ALL_PROXY", proxy_env_for(case.kind, ports));
        }
    }
}

fn scrub(cmd: &mut Command) {
    for key in PROXY_KEYS {
        cmd.env_remove(key);
    }
    cmd.env_remove("MIHOMO_SECRET");
    cmd.env_remove("TMUX");
}

fn assert_process_env(scenario: &str, pid: u32, ports: &Ports) -> Result<(), String> {
    let env = read_environ(pid)?;
    let case = matrix_cases()
        .into_iter()
        .find(|case| case.scenario == scenario)
        .ok_or(scenario.to_string())?;
    let http = env.get("HTTP_PROXY").map(String::as_str);
    let all = env.get("ALL_PROXY").map(String::as_str);
    let expected = proxy_env_for(case.kind, ports);
    match case.kind {
        common::ProxyKind::None => {
            if http.is_some() || all.is_some() {
                return Err(format!("{scenario} has a proxy env"));
            }
        }
        common::ProxyKind::Http | common::ProxyKind::HttpClosed => {
            if http != Some(expected.as_str()) {
                return Err(format!("{scenario} HTTP_PROXY {http:?} != {expected}"));
            }
        }
        common::ProxyKind::Socks => {
            if all != Some(expected.as_str()) {
                return Err(format!("{scenario} ALL_PROXY {all:?} != {expected}"));
            }
        }
    }
    Ok(())
}

fn read_environ(pid: u32) -> Result<std::collections::HashMap<String, String>, String> {
    let bytes =
        fs::read(format!("/proc/{pid}/environ")).map_err(|e| format!("environ {pid}: {e}"))?;
    let mut map = std::collections::HashMap::new();
    for entry in bytes.split(|byte| *byte == 0) {
        let Some((key, value)) = std::str::from_utf8(entry)
            .ok()
            .and_then(|text| text.split_once('='))
        else {
            continue;
        };
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

fn wait_ready(path: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if path.is_file() && fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(format!("timed out waiting for {}", path.display()))
}

fn wait_result(path: &Path) -> Result<ClientObs, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                return Ok(ClientObs {
                    scenario: value["scenario"].as_str().unwrap_or("").to_string(),
                    proxy_env: value["proxy_env"].as_str().unwrap_or("").to_string(),
                    connectivity: value["connectivity"].as_bool().unwrap_or(false),
                    egress_ip: value["egress_ip"].as_str().map(str::to_string),
                    pid: value["pid"].as_u64().unwrap_or(0) as u32,
                    comm: value["comm"].as_str().unwrap_or("").to_string(),
                    error: value["error"].as_str().map(str::to_string),
                });
            }
        }
        thread::sleep(Duration::from_millis(30));
    }
    Err(format!("no client result at {}", path.display()))
}

fn snapshot_until(lab: &Lab, api: u16, pids: &[u32]) -> Result<(Value, String, String), String> {
    let mut last = String::new();
    for _ in 0..8 {
        let (stdout, stderr) = once(lab, api, &[])?;
        last = stdout.clone();
        let value: Value = match serde_json::from_str(&stdout) {
            Ok(value) => value,
            Err(error) => return Err(format!("snapshot json {error}: {stderr}\n{stdout}")),
        };
        let seen = conn_rows(&value)?
            .into_iter()
            .filter_map(|row| row.pid)
            .collect::<Vec<_>>();
        if pids.iter().all(|pid| seen.contains(pid)) {
            let _ = stderr;
            return Ok((value, stdout, stderr));
        }
        thread::sleep(Duration::from_millis(200));
    }
    Err(format!(
        "snapshot never showed pids {pids:?}\n{last}\n{}",
        tail(&lab.dir.join("mihomo.log"))
    ))
}

fn once(lab: &Lab, api: u16, extra: &[&str]) -> Result<(String, String), String> {
    let output = mihomo_output(lab, api, extra)?;
    if !output.status.success() {
        return Err(format!(
            "mihomo-top --once failed\n{}{}",
            String::from_utf8_lossy(&output.stderr),
            tail(&lab.dir.join("mihomo.log"))
        ));
    }
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

fn mihomo_output(lab: &Lab, api: u16, extra: &[&str]) -> Result<std::process::Output, String> {
    let controller = format!("127.0.0.1:{api}");
    let secret = lab.dir.join("secret");
    let socket = lab.dir.join("tmux.sock");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mihomo-top"));
    cmd.args(["--once", "--controller", &controller, "--secret-file"])
        .arg(&secret)
        .arg("--socket")
        .arg(&socket)
        .args(extra);
    if !extra.contains(&"--timeout-secs") {
        cmd.args(["--timeout-secs", "8"]);
    }
    scrub(&mut cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| e.to_string())
}

fn assert_monitor(snapshot: &Value, api: u16, attribution: bool) -> Result<(), String> {
    if snapshot["schema"] != 1 {
        return Err(format!("schema {}", snapshot["schema"]));
    }
    if snapshot["connection_state"] != "connected" {
        return Err(format!(
            "state {} error {}",
            snapshot["connection_state"], snapshot["last_error"]
        ));
    }
    if snapshot["stale"] != false {
        return Err("snapshot is stale".into());
    }
    if snapshot["secret_in_use"] != true {
        return Err("secret_in_use is false".into());
    }
    if snapshot["local_attribution"] != json!(attribution) {
        return Err(format!(
            "local_attribution {}",
            snapshot["local_attribution"]
        ));
    }
    if snapshot["controller"] != format!("127.0.0.1:{api}") {
        return Err(format!("controller {}", snapshot["controller"]));
    }
    Ok(())
}

fn conn_rows(snapshot: &Value) -> Result<Vec<ConnRow>, String> {
    let Some(rows) = snapshot["connections"].as_array() else {
        return Err("connections is not an array".into());
    };
    Ok(rows
        .iter()
        .map(|row| ConnRow {
            pid: row["pid"].as_u64().map(|pid| pid as u32),
            process: row["process"].as_str().unwrap_or("").to_string(),
            chain: row["chain"].as_str().unwrap_or("").to_string(),
            destination_ip: row["destination_ip"].as_str().unwrap_or("").to_string(),
            destination_port: row["destination_port"].as_str().unwrap_or("").to_string(),
            attribution_status: row["attribution_status"].as_str().unwrap_or("").to_string(),
            session: row["session"].as_str().map(str::to_string),
            pane_id: row["pane_id"].as_str().map(str::to_string),
            network: row["network"].as_str().unwrap_or("").to_string(),
        })
        .collect())
}

fn capture_tui(lab: &Lab, api: u16) -> Result<String, String> {
    let socket = lab.dir.join("tmux.sock");
    let controller = format!("127.0.0.1:{api}");
    let secret = lab.dir.join("secret");
    let status = Command::new("tmux")
        .args([
            "-S",
            &socket.to_string_lossy(),
            "new-session",
            "-d",
            "-s",
            "view",
            "-x",
            "140",
            "-y",
            "32",
            "--",
        ])
        .arg(env!("CARGO_BIN_EXE_mihomo-top"))
        .arg("tui")
        .args(["--controller", &controller, "--secret-file"])
        .arg(&secret)
        .arg("--socket")
        .arg(&socket)
        .args(["--tick-ms", "100"])
        .env("TERM", "xterm-256color")
        .env_remove("MIHOMO_SECRET")
        .status()
        .map_err(|e| format!("tui tmux: {e}"))?;
    if !status.success() {
        return Err(format!("tui session failed: {status}"));
    }
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut last = String::new();
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(200));
        let output = Command::new("tmux")
            .args([
                "-S",
                &socket.to_string_lossy(),
                "capture-pane",
                "-t",
                "view",
                "-p",
                "-J",
            ])
            .output()
            .map_err(|e| e.to_string())?;
        last = String::from_utf8_lossy(&output.stdout).into_owned();
        if last.contains("mht-http") && last.contains("mock-upstream") {
            break;
        }
    }
    let _ = Command::new("tmux")
        .args([
            "-S",
            &socket.to_string_lossy(),
            "send-keys",
            "-t",
            "view",
            "q",
        ])
        .stderr(Stdio::null())
        .status();
    thread::sleep(Duration::from_millis(200));
    let _ = Command::new("tmux")
        .args([
            "-S",
            &socket.to_string_lossy(),
            "kill-session",
            "-t",
            "view",
        ])
        .stderr(Stdio::null())
        .status();
    Ok(last)
}

fn tun_probe() -> String {
    let output = Command::new("python3")
        .arg(lab_script())
        .arg("probe-tun")
        .output();
    match output {
        Ok(output) if output.status.success() => {
            format!(
                "tun probe: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            )
        }
        Ok(output) => format!(
            "tun probe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => format!("tun probe failed: {error}"),
    }
}

fn http_get(port: u16, path: &str, bearer: Option<&str>) -> Result<(u16, String), String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_millis(500)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let auth = match bearer {
        Some(secret) => format!("Authorization: Bearer {secret}\r\n"),
        None => String::new(),
    };
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n{auth}\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("short http from :{port}"))?;
    let status = head
        .split_whitespace()
        .nth(1)
        .ok_or("missing HTTP status")?
        .parse::<u16>()
        .map_err(|e| e.to_string())?;
    Ok((status, body.to_string()))
}

fn example_ipv4() -> Result<Ipv4Addr, String> {
    use std::net::ToSocketAddrs;
    ("example.com", 80)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .find_map(|addr| match addr.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
        .ok_or_else(|| "example.com has no IPv4 address".into())
}

fn save_artifact(name: &str, text: &str) {
    let dir = PathBuf::from("/opt/cursor/artifacts");
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join(name), redact_public_ips(text));
}

fn tail(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let start = text.len().saturating_sub(2000);
    text[start..].to_string()
}
