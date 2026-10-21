//! Offline end-to-end tests: fixtures bind only loopback and never use user tmux.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Server {
    address: String,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn start(frames: Vec<Value>, silent: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if server_stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
                            return;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_millis(250)))
                .unwrap();
            if silent {
                while !server_stop.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(10));
                }
                return;
            }
            let Ok(mut socket) = tungstenite::accept(stream) else {
                return;
            };
            let mut index = 0;
            while !server_stop.load(Ordering::Relaxed) {
                if socket
                    .send(tungstenite::Message::Text(
                        frames[index.min(frames.len() - 1)].to_string().into(),
                    ))
                    .is_err()
                {
                    break;
                }
                index += 1;
                for _ in 0..20 {
                    if server_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(handle),
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mihomo-top"));
        command
            .args(["--controller", &self.address, "--no-attribution"])
            .env_remove("MIHOMO_SECRET")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

struct Process(Child);
impl Process {
    fn finish(mut self, deadline: Duration) -> Output {
        let started = Instant::now();
        loop {
            if self.0.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                started.elapsed() < deadline,
                "child did not exit within {deadline:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
        // Child cannot be moved out of a Drop type; transfer pipe handles and
        // reap through the stored Child, with bounded fixture output sizes.
        let mut stdout = String::new();
        let mut stderr = String::new();
        use std::io::Read;
        if let Some(mut pipe) = self.0.stdout.take() {
            pipe.read_to_string(&mut stdout).unwrap();
        }
        if let Some(mut pipe) = self.0.stderr.take() {
            pipe.read_to_string(&mut stderr).unwrap();
        }
        Output {
            status: self.0.wait().unwrap(),
            stdout: stdout.into_bytes(),
            stderr: stderr.into_bytes(),
        }
    }
    fn terminate(&self) {
        assert!(Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status()
            .unwrap()
            .success());
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn empty() -> Value {
    json!({"uploadTotal":0,"downloadTotal":0,"connections":null})
}
fn active(host: String) -> Value {
    json!({"uploadTotal":12,"downloadTotal":34,"connections":[{"id":"fixture-connection","metadata":{"sourceIP":"127.0.0.1","sourcePort":"45000","destinationIP":"192.0.2.1","destinationPort":"443","network":"tcp","host":host},"upload":12,"download":34,"chains":["Node","Proxy"],"rule":"Match"}]})
}

#[test]
fn once_accepts_real_core_null_table_without_proc_or_tmux() {
    let server = Server::start(vec![empty()], false);
    let output =
        Process(server.command().arg("--once").spawn().unwrap()).finish(Duration::from_secs(3));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["connections"], json!([]));
    assert_eq!(value["local_attribution"], false);
    assert_eq!(value["stats_snapshot"]["proc_scans"], 0);
    assert_eq!(value["stats_snapshot"]["pane_refreshes"], 0);
    assert_eq!(value["stale"], false);
}

#[test]
fn non_tty_default_stream_clears_rows_and_stops_on_sigterm() {
    let server = Server::start(vec![active("example.test".into()), empty()], false);
    let mut child = Process(server.command().args(["--tick-ms", "100"]).spawn().unwrap());
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            let value: Value = serde_json::from_str(&line).unwrap();
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut saw_active = false;
    loop {
        assert!(Instant::now() < deadline);
        let value = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        if value["connection_count"] == 1 {
            saw_active = true;
        }
        if saw_active && value["connection_count"] == 0 && value["last_frame_at"].is_string() {
            assert_eq!(value["connections"], json!([]));
            assert_eq!(value["upload_total"], 0);
            break;
        }
    }
    child.terminate();
    assert!(child.finish(Duration::from_secs(2)).status.success());
    reader.join().unwrap();
}

#[test]
fn malformed_envelope_is_an_error_without_stdout_facts() {
    let server = Server::start(vec![json!({"connections":null})], false);
    let output = Process(
        server
            .command()
            .args(["--once", "--timeout-secs", "1"])
            .spawn()
            .unwrap(),
    )
    .finish(Duration::from_secs(3));
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("JSON data error"));
}

#[test]
fn sigterm_cancels_silent_handshake() {
    let server = Server::start(vec![empty()], true);
    let child = Process(server.command().arg("--stream").spawn().unwrap());
    thread::sleep(Duration::from_millis(150));
    child.terminate();
    assert!(child.finish(Duration::from_secs(2)).status.success());
}

#[test]
fn sigterm_exits_even_when_stdout_pipe_is_full() {
    let server = Server::start(vec![active("x".repeat(128 * 1024))], false);
    let child = Process(
        server
            .command()
            .args(["--stream", "--tick-ms", "100"])
            .spawn()
            .unwrap(),
    );
    // Deliberately do not drain stdout. A full pipe must not prevent shutdown.
    thread::sleep(Duration::from_millis(500));
    child.terminate();
    assert!(child.finish(Duration::from_secs(2)).status.success());
}

#[test]
fn closed_output_pipe_is_a_clean_exit() {
    let server = Server::start(vec![empty()], false);
    let mut child = Process(
        server
            .command()
            .args(["--stream", "--tick-ms", "100"])
            .spawn()
            .unwrap(),
    );
    drop(child.0.stdout.take());
    assert!(child.finish(Duration::from_secs(2)).status.success());
}
