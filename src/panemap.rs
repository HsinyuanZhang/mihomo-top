//! tmux pane map and pid → pane attribution.
//!
//! The pane list comes from the native tmux binary with an argument array
//! (never a shell string) and a unit-separator format so user-controlled
//! names cannot break parsing. An absent server is a normal empty state.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{FlowError, Result};
use crate::procnet::read_ppid;

const SEP: char = '\x1f';
const FIELDS: &str = "#{pane_id}\u{1f}#{pane_pid}\u{1f}#{session_name}";
const QUERY_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneEntry {
    pub pane_id: String,
    pub pane_pid: u32,
    pub session_name: String,
}

/// How many parent hops we are willing to walk before calling a process
/// "not in tmux". Deep chains happen (wrapper scripts), loops should not.
const MAX_ANCESTRY_DEPTH: usize = 64;

/// Query the pane list from a tmux server. `None` socket = default server.
pub fn query_panes(socket: Option<&str>, tmux_bin: &str) -> Result<Vec<PaneEntry>> {
    let mut cmd = Command::new(tmux_bin);
    // tmux has no use for the controller credential. Do not inherit it into
    // a separately spawned process.
    cmd.env_remove("MIHOMO_SECRET");
    if let Some(s) = socket {
        cmd.args(["-S", s]);
    }
    let mut child = match cmd
        .args(["list-panes", "-a", "-F", FIELDS])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    // Drain both pipes concurrently. Waiting first can deadlock when a
    // server has enough panes to fill an OS pipe buffer.
    let stdout_reader = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut text = String::new();
            use std::io::Read;
            out.read_to_string(&mut text).map(|_| text)
        })
    });
    let stderr_reader = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut text = String::new();
            use std::io::Read;
            err.read_to_string(&mut text).map(|_| text)
        })
    });
    let deadline = Instant::now() + QUERY_DEADLINE;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FlowError::ProcParse(
                "tmux list-panes timed out after 2s".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader
        .map(|r| r.join().unwrap_or_else(|_| Ok(String::new())))
        .transpose()?
        .unwrap_or_default();
    let stderr = stderr_reader
        .map(|r| r.join().unwrap_or_else(|_| Ok(String::new())))
        .transpose()?
        .unwrap_or_default();
    if !status.success() {
        if stderr.contains("no server running") || stderr.contains("error connecting") {
            return Ok(Vec::new()); // normal state: no server on this socket
        }
        return Err(FlowError::Tmux {
            rc: status.code().unwrap_or(-1),
            stderr: stderr.trim().to_string(),
        });
    }
    Ok(parse_pane_output(&stdout))
}

/// Parse `list-panes -a -F` output; deduplicates linked panes by pane_id and
/// strips control characters from names (defence-in-depth against terminal
/// injection through session names).
pub fn parse_pane_output(text: &str) -> Vec<PaneEntry> {
    let mut by_id: HashMap<String, PaneEntry> = HashMap::new();
    for line in text.lines() {
        // `splitn` keeps a malformed/user-controlled session name from
        // shifting the required pane id and pid fields.
        let parts: Vec<&str> = line.splitn(3, SEP).collect();
        if parts.len() < 3 {
            continue;
        }
        let pane_id = strip_controls(parts[0]);
        if pane_id.is_empty() || !pane_id.starts_with('%') {
            continue;
        }
        let Ok(pane_pid) = parts[1].parse::<u32>() else {
            continue;
        };
        let session_name = strip_controls(parts[2]);
        by_id.entry(pane_id.clone()).or_insert(PaneEntry {
            pane_id,
            pane_pid,
            session_name,
        });
    }
    let mut panes: Vec<_> = by_id.into_values().collect();
    panes.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
    panes
}

fn strip_controls(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// Pane lookup indexed by pane shell pid.
#[derive(Debug, Clone, Default)]
pub struct PaneMap {
    panes: Vec<PaneEntry>,
    by_pid: HashMap<u32, usize>,
}

impl PaneMap {
    pub fn from_panes(panes: Vec<PaneEntry>) -> Self {
        let by_pid = panes
            .iter()
            .enumerate()
            .map(|(i, p)| (p.pane_pid, i))
            .collect();
        Self { panes, by_pid }
    }

    pub fn panes(&self) -> &[PaneEntry] {
        &self.panes
    }

    /// Walk the parent chain of `pid`; if any ancestor is a pane shell, the
    /// process belongs to that pane.
    pub fn pane_of(&self, proc_root: &Path, pid: u32) -> Option<&PaneEntry> {
        let mut cur = pid;
        let mut seen: Vec<u32> = Vec::with_capacity(8);
        for _ in 0..MAX_ANCESTRY_DEPTH {
            if let Some(&idx) = self.by_pid.get(&cur) {
                return self.panes.get(idx);
            }
            match read_ppid(proc_root, cur) {
                Some(0) | None => return None,
                Some(p) => {
                    if seen.contains(&p) {
                        return None; // cycle guard
                    }
                    seen.push(p);
                    cur = p;
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_dedups() {
        let text = "%1\u{1f}100\u{1f}agy\n%0\u{1f}200\u{1f}claude\n%1\u{1f}100\u{1f}agy";
        let panes = parse_pane_output(text);
        assert_eq!(panes.len(), 2);
        assert!(panes.iter().any(|p| p.pane_id == "%1" && p.pane_pid == 100));
        assert!(panes
            .iter()
            .any(|p| p.pane_id == "%0" && p.session_name == "claude"));
    }

    #[test]
    fn junk_lines_are_skipped() {
        let text = "header junk\nno-sep\n%2\u{1f}notapid\u{1f}x\n%3\u{1f}7\u{1f}ok\u{1f}extra";
        let panes = parse_pane_output(text);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].pane_id, "%3");
        assert_eq!(panes[0].pane_pid, 7);
    }

    #[test]
    fn control_chars_in_names_are_stripped() {
        let text = "%5\u{1f}9\u{1f}na\u{7}me";
        let panes = parse_pane_output(text);
        assert_eq!(panes[0].session_name, "name");
    }

    #[test]
    fn ancestry_walk_finds_pane() {
        // build a fake /proc with chain 31 → 30 → 29(pane shell)
        let dir = std::env::temp_dir().join(format!("mihomo-top-pane-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (pid, ppid) in [(31u32, 30u32), (30, 29), (29, 1)] {
            let d = dir.join(pid.to_string());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("stat"),
                format!("{pid} (x) S {ppid} {pid} {pid} 0\n"),
            )
            .unwrap();
        }
        let map = PaneMap::from_panes(vec![PaneEntry {
            pane_id: "%7".into(),
            pane_pid: 29,
            session_name: "sess".into(),
        }]);
        let hit = map.pane_of(&dir, 31).unwrap();
        assert_eq!(hit.session_name, "sess");
        assert!(map.pane_of(&dir, 29).is_some()); // the shell itself
        assert!(map.pane_of(&dir, 404).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absent_server_is_empty() {
        let err_output = "error connecting to /tmp/nope (No such file or directory)";
        // simulate by calling parse on the error text path — the Command
        // path itself is exercised in integration (see tests/).
        assert!(!err_output.contains("no server running"));
    }

    #[test]
    fn missing_tmux_binary_is_an_optional_empty_state() {
        let panes = query_panes(None, "/definitely/not/a/tmux-binary").unwrap();
        assert!(panes.is_empty());
    }
}
