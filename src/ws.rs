//! Bounded synchronous reader for the controller's `/connections` stream.

use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::client::ClientHandshake;
use tungstenite::handshake::HandshakeError;
use tungstenite::http::header::AUTHORIZATION;
use tungstenite::http::Uri;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Error as WsError, Message, WebSocket};

use crate::error::log;
use crate::model::ConnectionsFrame;

/// Events handed to the aggregator. Parse failures must not look like frames.
pub enum WsEvent {
    Frame(Box<ConnectionsFrame>),
    Connected,
    Disconnected(String),
    ParseError(String),
}

const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const HANDSHAKE_TICK: Duration = Duration::from_millis(25);
const READ_TIMEOUT: Duration = Duration::from_millis(250);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(15);
const MAX_MESSAGE_SIZE: usize = 8 * 1024 * 1024;

pub fn spawn(
    url: String,
    secret: Option<String>,
    stop: Arc<AtomicBool>,
    tx: SyncSender<WsEvent>,
    stats: Arc<crate::state::Stats>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("ws-reader".into())
        .spawn(move || {
            let mut backoff = BACKOFF_MIN;
            let mut reconnecting = false;
            while !stop.load(Ordering::Relaxed) {
                if reconnecting {
                    // Count an actual new connection attempt, not merely a
                    // disconnected notification or a cancelled backoff.
                    stats.ws_reconnects.fetch_add(1, Ordering::Relaxed);
                }
                let cycle = run_one_connection(&url, secret.as_deref(), &stop, &tx, &stats);
                let Cycle::Ended { delivered, reason } = cycle else {
                    break;
                };
                if delivered {
                    backoff = BACKOFF_MIN;
                }
                if !publish(&tx, WsEvent::Disconnected(reason.clone()), &stats) {
                    break;
                }
                log("ws", format!("{reason}; retrying in {backoff:?}"));
                if wait_or_stop(&stop, backoff) {
                    break;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
                reconnecting = true;
            }
            log("ws", "reader thread exiting");
        })
        .expect("spawn ws-reader thread")
}

enum Cycle {
    Stopped,
    Ended { delivered: bool, reason: String },
}

fn run_one_connection(
    url: &str,
    secret: Option<&str>,
    stop: &AtomicBool,
    tx: &SyncSender<WsEvent>,
    stats: &crate::state::Stats,
) -> Cycle {
    let mut ws = match connect(url, secret, stop) {
        Ok(Some(ws)) => ws,
        Ok(None) => return Cycle::Stopped,
        Err(reason) => {
            return Cycle::Ended {
                delivered: false,
                reason,
            }
        }
    };
    if !publish(tx, WsEvent::Connected, stats) {
        return Cycle::Stopped;
    }
    log("ws", "connected");
    let mut delivered = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = ws.close(None);
            return Cycle::Stopped;
        }
        match ws.read() {
            Ok(Message::Text(text)) => {
                if !deliver(text.as_bytes(), tx, &mut delivered, stats) {
                    return Cycle::Stopped;
                }
            }
            Ok(Message::Binary(bytes)) => {
                if !deliver(&bytes, tx, &mut delivered, stats) {
                    return Cycle::Stopped;
                }
            }
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
            Ok(Message::Close(_)) => {
                return Cycle::Ended {
                    delivered,
                    reason: "stream closed by server".into(),
                };
            }
            Err(WsError::Io(ref e)) if is_timeout(e) => {}
            Err(e) => {
                return Cycle::Ended {
                    delivered,
                    reason: websocket_error_reason(&e, "read"),
                }
            }
        }
    }
}

/// `Ok(None)` means cancellation was observed during the handshake.
fn connect(
    url: &str,
    secret: Option<&str>,
    stop: &AtomicBool,
) -> Result<Option<WebSocket<TcpStream>>, String> {
    let uri: Uri = url
        .parse()
        .map_err(|_| "invalid controller URL".to_string())?;
    if uri.scheme_str() != Some("ws") {
        return Err("controller URL must use ws".into());
    }
    let host = uri
        .host()
        .ok_or_else(|| "controller URL has no host".to_string())?;
    // `http::Uri` normally returns an IPv6 host without brackets, but accept
    // either representation so `[::1]:PORT` remains a valid local controller.
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let ip: IpAddr = host
        .parse()
        .map_err(|_| "controller host must be a numeric loopback address".to_string())?;
    if !ip.is_loopback() {
        return Err("controller host must be a loopback address".into());
    }
    let addr = SocketAddr::new(ip, uri.port_u16().unwrap_or(80));
    if stop.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("controller connect failed: {}", io_kind(e.kind())))?;
    stream
        .set_nodelay(true)
        .map_err(|_| "controller socket setup failed".to_string())?;
    stream
        .set_nonblocking(true)
        .map_err(|_| "controller socket setup failed".to_string())?;
    let mut request = uri
        .into_client_request()
        .map_err(|_| "invalid controller request".to_string())?;
    if let Some(secret) = secret {
        let value = format!("Bearer {secret}");
        request.headers_mut().insert(
            AUTHORIZATION,
            value
                .parse()
                .map_err(|_| "invalid controller authorization".to_string())?,
        );
    }
    let config = WebSocketConfig::default()
        .read_buffer_size(64 * 1024)
        .write_buffer_size(16 * 1024)
        .max_write_buffer_size(64 * 1024)
        .max_message_size(Some(MAX_MESSAGE_SIZE))
        .max_frame_size(Some(MAX_MESSAGE_SIZE));
    let mut mid = ClientHandshake::start(stream, request, Some(config))
        .map_err(|e| websocket_error_reason(&e, "handshake"))?;
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match mid.handshake() {
            Ok((ws, _)) => {
                ws.get_ref()
                    .set_nonblocking(false)
                    .map_err(|_| "controller socket setup failed".to_string())?;
                ws.get_ref()
                    .set_read_timeout(Some(READ_TIMEOUT))
                    .map_err(|_| "controller socket setup failed".to_string())?;
                ws.get_ref()
                    .set_write_timeout(Some(READ_TIMEOUT))
                    .map_err(|_| "controller socket setup failed".to_string())?;
                return Ok(Some(ws));
            }
            Err(HandshakeError::Interrupted(next)) => {
                if Instant::now() >= deadline {
                    return Err("websocket handshake timed out".into());
                }
                mid = next;
                if wait_or_stop(stop, HANDSHAKE_TICK) {
                    return Ok(None);
                }
            }
            Err(HandshakeError::Failure(e)) => return Err(websocket_error_reason(&e, "handshake")),
        }
    }
}

fn deliver(
    bytes: &[u8],
    tx: &SyncSender<WsEvent>,
    delivered: &mut bool,
    stats: &crate::state::Stats,
) -> bool {
    match parse_frame(bytes) {
        Ok(mut frame) => {
            frame.received_at = Some(chrono::Utc::now());
            *delivered = true;
            publish(tx, WsEvent::Frame(Box::new(frame)), stats)
        }
        Err(reason) => {
            stats.parse_errors.fetch_add(1, Ordering::Relaxed);
            publish(tx, WsEvent::ParseError(reason), stats)
        }
    }
}

fn parse_frame(bytes: &[u8]) -> Result<ConnectionsFrame, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(json_error_reason)?;
    crate::model::validate_envelope(&value)
        .map_err(|reason| format!("invalid connections envelope: {reason}"))?;
    serde_json::from_value(value).map_err(json_error_reason)
}

/// Never block the socket producer. A closed consumer ends this reader.
fn publish(tx: &SyncSender<WsEvent>, event: WsEvent, stats: &crate::state::Stats) -> bool {
    match tx.try_send(event) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            stats.dropped_events.fetch_add(1, Ordering::Relaxed);
            true
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

fn wait_or_stop(stop: &AtomicBool, duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep((deadline - now).min(HANDSHAKE_TICK));
    }
    true
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

fn io_kind(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::TimedOut => "timed out",
        ErrorKind::ConnectionRefused => "connection refused",
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => "connection reset",
        ErrorKind::NotConnected => "not connected",
        _ => "I/O error",
    }
}

/// Do not expose peer headers/bodies, source JSON, or the authorization token.
fn websocket_error_reason(error: &WsError, phase: &str) -> String {
    match error {
        WsError::Http(response) => {
            format!("websocket {phase} rejected: HTTP {}", response.status())
        }
        WsError::Io(e) => format!("websocket {phase} failed: {}", io_kind(e.kind())),
        WsError::Capacity(_) => format!("websocket {phase} failed: message too large"),
        WsError::ConnectionClosed => format!("websocket {phase}: connection closed"),
        _ => format!("websocket {phase} failed"),
    }
}

fn json_error_reason(error: serde_json::Error) -> String {
    let category = match error.classify() {
        serde_json::error::Category::Io => "I/O",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "incomplete input",
    };
    format!(
        "JSON {category} error at line {}, column {}",
        error.line(),
        error.column()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::sync_channel;
    use std::thread;

    /// A loopback-only server stream which deliberately pauses after a short
    /// handshake response write. This exercises the client's resumable
    /// MidHandshake rather than a buffered in-memory parser.
    struct DelayedWriteStream {
        stream: TcpStream,
        delayed: bool,
    }

    impl DelayedWriteStream {
        fn new(stream: TcpStream) -> Self {
            Self {
                stream,
                delayed: false,
            }
        }
    }

    impl Read for DelayedWriteStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.stream.read(buf)
        }
    }

    impl Write for DelayedWriteStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if !self.delayed {
                self.delayed = true;
                let count = self.stream.write(&buf[..buf.len().min(17)])?;
                self.stream.flush()?;
                thread::sleep(READ_TIMEOUT + Duration::from_millis(75));
                Ok(count)
            } else {
                self.stream.write(buf)
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.stream.flush()
        }
    }

    fn fixture<F>(handler: F) -> (String, thread::JoinHandle<()>)
    where
        F: FnOnce(TcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handler(stream);
        });
        (format!("ws://{address}/connections"), handle)
    }

    fn server_text_frame(payload: &[u8]) -> Vec<u8> {
        assert!((126..=u16::MAX as usize).contains(&payload.len()));
        let mut frame = vec![0x81, 126];
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn null_connection_table_is_delivered_as_empty_frame() {
        let frame =
            parse_frame(br#"{"uploadTotal":7,"downloadTotal":0,"connections":null}"#).unwrap();
        assert_eq!(frame.upload_total, 7);
        assert!(frame.connections.is_empty());
    }

    #[test]
    fn missing_envelope_is_a_parse_error_not_an_empty_frame() {
        let error = parse_frame(br#"{"uploadTotal":7}"#).unwrap_err();
        assert_eq!(
            error,
            "invalid connections envelope: connections key missing (not a connections frame?)"
        );
    }

    #[test]
    fn json_error_does_not_repeat_input_values() {
        let error = parse_frame(br#"{"connections":["very-secret-input"#).unwrap_err();
        assert!(error.starts_with("JSON incomplete input error at line "));
        assert!(!error.contains("very-secret-input"));
    }

    #[test]
    fn http_rejection_exposes_status_but_not_headers_or_body() {
        let response = tungstenite::http::Response::builder()
            .status(401)
            .header("x-private", "do-not-expose")
            .body(Some(b"body-token".to_vec()))
            .unwrap();
        let reason = websocket_error_reason(&WsError::Http(response), "handshake");
        assert_eq!(
            reason,
            "websocket handshake rejected: HTTP 401 Unauthorized"
        );
        assert!(!reason.contains("private"));
        assert!(!reason.contains("token"));
    }

    #[test]
    fn full_queue_drops_without_blocking() {
        let (tx, _rx) = sync_channel(1);
        let stats = crate::state::Stats::default();
        assert!(publish(&tx, WsEvent::Connected, &stats));
        let start = Instant::now();
        assert!(publish(&tx, WsEvent::Disconnected("closed".into()), &stats));
        assert!(start.elapsed() < Duration::from_millis(50));
        assert_eq!(stats.dropped_events.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn closed_receiver_stops_the_reader_path() {
        let (tx, rx) = sync_channel(1);
        drop(rx);
        assert!(!publish(
            &tx,
            WsEvent::Connected,
            &crate::state::Stats::default()
        ));
    }

    #[test]
    fn stop_interrupts_backoff_without_waiting_for_its_deadline() {
        let stop = Arc::new(AtomicBool::new(false));
        let stopper = Arc::clone(&stop);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            stopper.store(true, Ordering::Relaxed);
        });
        let start = Instant::now();
        assert!(wait_or_stop(&stop, Duration::from_secs(1)));
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn non_loopback_and_dns_hosts_are_rejected_before_connecting() {
        let stop = AtomicBool::new(false);
        assert_eq!(
            connect("ws://example.com:9/connections", None, &stop).unwrap_err(),
            "controller host must be a numeric loopback address"
        );
        assert_eq!(
            connect("ws://192.0.2.1:9/connections", None, &stop).unwrap_err(),
            "controller host must be a loopback address"
        );
    }

    #[test]
    fn ipv6_loopback_controller_handshakes_and_delivers_a_frame() {
        let listener = match TcpListener::bind("[::1]:0") {
            Ok(listener) => listener,
            Err(error)
                if error.kind() == ErrorKind::AddrNotAvailable
                    || matches!(error.raw_os_error(), Some(97) | Some(47) | Some(10047)) =>
            {
                // EAFNOSUPPORT on Linux, macOS/BSD, and Windows respectively.
                return;
            }
            Err(error) => panic!("bind IPv6 loopback fixture: {error}"),
        };
        let url = format!(
            "ws://[::1]:{}/connections",
            listener.local_addr().unwrap().port()
        );
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.send(Message::Text(
                r#"{"uploadTotal":6,"downloadTotal":7,"connections":null}"#.into(),
            ))
            .unwrap();
            thread::sleep(Duration::from_millis(100));
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = sync_channel(4);
        let reader = spawn(
            url,
            None,
            Arc::clone(&stop),
            tx,
            Arc::new(crate::state::Stats::default()),
        );
        let received = matches!(
            rx.recv_timeout(Duration::from_secs(1)),
            Ok(WsEvent::Connected)
        ) && matches!(
            rx.recv_timeout(Duration::from_secs(1)),
            Ok(WsEvent::Frame(frame)) if frame.upload_total == 6 && frame.download_total == 7
        );
        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        server.join().unwrap();
        assert!(received);
    }

    #[test]
    fn silent_handshake_is_cancelled_within_one_read_tick() {
        let (url, server) = fixture(|mut stream| {
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            thread::sleep(Duration::from_secs(1));
        });
        let stop = Arc::new(AtomicBool::new(false));
        let cancelling_stop = Arc::clone(&stop);
        let client = thread::spawn(move || connect(&url, None, &cancelling_stop));
        thread::sleep(Duration::from_millis(75));
        let start = Instant::now();
        stop.store(true, Ordering::Relaxed);
        assert!(client.join().unwrap().unwrap().is_none());
        assert!(start.elapsed() < Duration::from_millis(500));
        server.join().unwrap();
    }

    #[test]
    fn stop_interrupts_an_idle_websocket_read_within_one_read_tick() {
        let (url, server) = fixture(|stream| {
            let _ws = tungstenite::accept(stream).unwrap();
            thread::sleep(Duration::from_secs(1));
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = sync_channel(2);
        let reader = spawn(
            url,
            None,
            Arc::clone(&stop),
            tx,
            Arc::new(crate::state::Stats::default()),
        );
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)),
            Ok(WsEvent::Connected)
        ));
        let start = Instant::now();
        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        assert!(start.elapsed() < Duration::from_millis(500));
        server.join().unwrap();
    }

    #[test]
    fn partial_handshake_extended_header_and_payload_resume_after_timeouts() {
        let (url, server) = fixture(|stream| {
            let mut ws = tungstenite::accept(DelayedWriteStream::new(stream)).unwrap();
            let payload = format!(
                r#"{{"uploadTotal":9,"downloadTotal":0,"connections":null,"padding":"{}"}}"#,
                "x".repeat(260)
            );
            let frame = server_text_frame(payload.as_bytes());
            let stream = &mut ws.get_mut().stream;
            // Send the extended-length prefix, then part of the payload, with
            // read timeouts between each piece.
            stream.write_all(&frame[..2]).unwrap();
            stream.flush().unwrap();
            thread::sleep(READ_TIMEOUT + Duration::from_millis(75));
            stream.write_all(&frame[2..11]).unwrap();
            stream.flush().unwrap();
            thread::sleep(READ_TIMEOUT + Duration::from_millis(75));
            stream.write_all(&frame[11..]).unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(100));
        });
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(crate::state::Stats::default());
        let (tx, rx) = sync_channel(8);
        let reader = spawn(url, None, Arc::clone(&stop), tx, Arc::clone(&stats));
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut frame = None;
        while Instant::now() < deadline {
            if let Ok(WsEvent::Frame(value)) = rx.recv_timeout(Duration::from_millis(100)) {
                frame = Some(value);
                break;
            }
        }
        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        server.join().unwrap();
        let frame = frame.expect("frame survives partial handshake/header/payload timeouts");
        assert_eq!(frame.upload_total, 9);
        assert!(frame.connections.is_empty());
        assert_eq!(stats.parse_errors.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn http_fixture_never_leaks_response_header_or_body() {
        let (url, server) = fixture(|mut stream| {
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nX-Secret: HEADER_MARKER\r\nContent-Length: 11\r\n\r\nBODY_MARKER")
                .unwrap();
        });
        let error = connect(&url, Some("REQUEST_SECRET"), &AtomicBool::new(false)).unwrap_err();
        server.join().unwrap();
        assert_eq!(error, "websocket handshake rejected: HTTP 401 Unauthorized");
        assert!(!error.contains("MARKER"));
        assert!(!error.contains("REQUEST_SECRET"));
    }

    #[test]
    fn reconnect_counter_tracks_a_real_second_connection_attempt() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/connections", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            // The first session succeeds, then closes without a frame.
            let (first, _) = listener.accept().unwrap();
            drop(tungstenite::accept(first).unwrap());
            // The reader's one-second initial backoff must complete before
            // this second accept; it then delivers a normal frame.
            let (second, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(second).unwrap();
            ws.send(Message::Text(
                r#"{"uploadTotal":3,"downloadTotal":4,"connections":null}"#.into(),
            ))
            .unwrap();
            thread::sleep(Duration::from_millis(100));
        });
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(crate::state::Stats::default());
        let (tx, rx) = sync_channel(8);
        let reader = spawn(url, None, Arc::clone(&stop), tx, Arc::clone(&stats));
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut received = false;
        while Instant::now() < deadline {
            if matches!(
                rx.recv_timeout(Duration::from_millis(100)),
                Ok(WsEvent::Frame(_))
            ) {
                received = true;
                break;
            }
        }
        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        server.join().unwrap();
        assert!(received);
        assert_eq!(stats.ws_reconnects.load(Ordering::Relaxed), 1);
    }
}
