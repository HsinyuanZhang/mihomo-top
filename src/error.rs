//! Error type. Library code returns `FlowError`; `main` converts to
//! `anyhow::Error` for reporting.

use std::fmt;

#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    // Boxed: tungstenite::Error is large and would bloat every Result.
    #[error("websocket error: {0}")]
    Ws(Box<tungstenite::Error>),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("tmux failed (rc={rc}): {stderr}")]
    Tmux { rc: i32, stderr: String },

    #[error("invalid config: {0}")]
    Config(String),

    #[error("/proc parse error: {0}")]
    ProcParse(String),
}

impl From<tungstenite::Error> for FlowError {
    fn from(e: tungstenite::Error) -> Self {
        Self::Ws(Box::new(e))
    }
}

pub type Result<T, E = FlowError> = std::result::Result<T, E>;

/// In TUI mode stderr goes to the same terminal: a stray log line would
/// corrupt the rendered screen (raw mode prints it at the cursor). The
/// binary silences library logs there via this switch.
static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, std::sync::atomic::Ordering::Relaxed);
}

/// Thread-safe stderr logging without pulling a logging framework.
/// No-ops when quieted (TUI mode).
pub fn log(component: &str, msg: impl fmt::Display) {
    if !QUIET.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("[mihomo-top:{component}] {msg}");
    }
}
