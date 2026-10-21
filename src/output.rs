//! Versioned machine-readable snapshots. Counters are cumulative bytes, not rates.

use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use std::io::{self, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use serde::Serialize;

use crate::state::Dashboard;

#[derive(Serialize)]
pub struct Snapshot<'a> {
    pub schema: u32,
    pub generated: String,
    pub scope_note: &'static str,
    #[serde(flatten)]
    pub dashboard: &'a Dashboard,
}

pub fn write_snapshot(
    writer: &mut impl Write,
    dashboard: &Dashboard,
    pretty: bool,
) -> io::Result<()> {
    let snapshot = Snapshot {
        schema: 1,
        generated: chrono::Utc::now().to_rfc3339(),
        scope_note: crate::ui::SCOPE_NOTE,
        dashboard,
    };
    let mut bytes = if pretty {
        serde_json::to_vec_pretty(&snapshot)?
    } else {
        serde_json::to_vec(&snapshot)?
    };
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()
}

/// Nonblocking stdout keeps signal shutdown responsive even if a pipe stops
/// draining. Original file status flags are restored when this scope ends.
pub struct CancellableStdout {
    stdout: io::Stdout,
    original: OFlags,
    stop: Arc<AtomicBool>,
}

impl CancellableStdout {
    pub fn new(stop: Arc<AtomicBool>) -> io::Result<Self> {
        let stdout = io::stdout();
        let original = fcntl_getfl(&stdout)?;
        fcntl_setfl(&stdout, original | OFlags::NONBLOCK)?;
        Ok(Self {
            stdout,
            original,
            stop,
        })
    }
}

impl Write for CancellableStdout {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "shutdown requested",
                ));
            }
            match rustix::io::write(&self.stdout, bytes).map_err(io::Error::from) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }

    fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        // The std default retries Interrupted forever; cancellation must escape.
        while !bytes.is_empty() {
            let written = self.write(bytes)?;
            if written == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = &bytes[written..];
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for CancellableStdout {
    fn drop(&mut self) {
        let _ = fcntl_setfl(&self.stdout, self.original);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_is_one_versioned_json_document_per_line() {
        let dashboard = Dashboard::default();
        let mut out = Vec::new();
        write_snapshot(&mut out, &dashboard, false).unwrap();
        write_snapshot(&mut out, &dashboard, false).unwrap();
        let output = String::from_utf8(out).unwrap();
        assert_eq!(output.lines().count(), 2);
        for line in output.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["schema"], 1);
            assert!(value.get("generated").is_some());
            assert!(value.get("connection_state").is_some());
        }
    }
}
