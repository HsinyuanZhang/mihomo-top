//! Frame aggregation and conservative local process attribution.
use crate::model::{ConnectionEntry, ConnectionsFrame};
use crate::panemap::{query_panes, PaneEntry, PaneMap};
use crate::procnet::{proc_exists, read_start_time, ProcSnapshot};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
const PANE_REFRESH: Duration = Duration::from_secs(5);
const UNRESOLVED_RETRY: Duration = Duration::from_secs(1);
pub trait ProcSource {
    fn snapshot(&self) -> ProcSnapshot;
}
pub trait PaneSource {
    fn panes(&self) -> Vec<PaneEntry>;
}
pub struct RealProcSource {
    root: PathBuf,
}
impl RealProcSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}
impl ProcSource for RealProcSource {
    fn snapshot(&self) -> ProcSnapshot {
        crate::procnet::snapshot(&self.root).unwrap_or_else(|e| {
            crate::error::log("proc", format!("snapshot failed: {e}"));
            ProcSnapshot {
                error: Some(e.to_string()),
                ..Default::default()
            }
        })
    }
}
pub struct RealPaneSource {
    socket: Option<String>,
    tmux_bin: String,
}
impl RealPaneSource {
    pub fn new(socket: Option<String>, tmux_bin: impl Into<String>) -> Self {
        Self {
            socket,
            tmux_bin: tmux_bin.into(),
        }
    }
}
impl PaneSource for RealPaneSource {
    fn panes(&self) -> Vec<PaneEntry> {
        query_panes(self.socket.as_deref(), &self.tmux_bin).unwrap_or_else(|e| {
            crate::error::log("tmux", format!("query failed: {e}"));
            Vec::new()
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaneRef {
    pub session_name: String,
    pub pane_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum AttributionStatus {
    Resolved,
    Unknown,
    Disabled,
    Unsupported,
    Ambiguous,
}
impl AttributionStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Unknown => "unknown",
            Self::Disabled => "disabled",
            Self::Unsupported => "unsupported",
            Self::Ambiguous => "ambiguous",
        }
    }
}
#[derive(Debug, Clone)]
struct Attribution {
    pid: Option<u32>,
    pid_start_time: Option<u64>,
    comm: String,
    pane: Option<PaneRef>,
    status: AttributionStatus,
    reason: Option<String>,
    last_attempt: Instant,
}
impl Attribution {
    fn unresolved(status: AttributionStatus, reason: Option<&str>) -> Self {
        Self {
            pid: None,
            pid_start_time: None,
            comm: String::new(),
            pane: None,
            status,
            reason: reason.map(str::to_owned),
            last_attempt: Instant::now(),
        }
    }
    fn disabled() -> Self {
        Self::unresolved(
            AttributionStatus::Disabled,
            Some("local attribution disabled"),
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortMode {
    #[default]
    BytesDesc,
    NewestFirst,
}
#[derive(Debug, Clone, Serialize, Default)]
pub struct Row {
    pub host: String,
    pub destination_port: String,
    pub network: String,
    pub process: String,
    pub session: Option<String>,
    pub pane_id: Option<String>,
    pub rule: String,
    pub chain: String,
    pub upload: u64,
    pub download: u64,
    pub age_seconds: Option<i64>,
    pub pid: Option<u32>,
    pub id: String,
    pub source_ip: String,
    pub source_port: String,
    pub destination_ip: String,
    pub chains: Vec<String>,
    pub rule_payload: String,
    pub attribution_status: String,
    pub attribution_reason: Option<String>,
    pub pid_start_time: Option<u64>,
}
#[derive(Debug, Default, Serialize)]
pub struct Stats {
    pub frames: AtomicU64,
    pub proc_scans: AtomicU64,
    pub pane_refreshes: AtomicU64,
    pub ws_reconnects: AtomicU64,
    pub unresolved_sockets: AtomicU64,
    pub dropped_events: AtomicU64,
    pub parse_errors: AtomicU64,
}
#[derive(Debug, Clone, Serialize, Default)]
pub struct StatsSnapshot {
    pub frames: u64,
    pub proc_scans: u64,
    pub pane_refreshes: u64,
    pub ws_reconnects: u64,
    pub unresolved_sockets: u64,
    pub dropped_events: u64,
    pub parse_errors: u64,
}
#[derive(Debug, Clone, Serialize, Default)]
pub struct Dashboard {
    pub controller: String,
    pub secret_in_use: bool,
    pub upload_total: u64,
    pub download_total: u64,
    pub connection_count: usize,
    #[serde(rename = "connections")]
    pub rows: Vec<Row>,
    pub sort_mode: SortMode,
    pub last_frame_at: Option<String>,
    pub attribution_notes: Vec<String>,
    pub stats_snapshot: StatsSnapshot,
    pub connection_state: String,
    pub last_error: Option<String>,
    pub stale: bool,
    pub local_attribution: bool,
}
/// Core connection identity: id + protocol + canonical source endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConnectionKey {
    id: String,
    protocol: String,
    source: SocketAddr,
}
pub struct Aggregator<P: ProcSource, T: PaneSource> {
    proc_source: P,
    pane_source: T,
    proc_root: PathBuf,
    cache: HashMap<ConnectionKey, Attribution>,
    pane_map: PaneMap,
    last_pane_refresh: Option<Instant>,
    stats: Arc<Stats>,
    attribution_enabled: bool,
    panes_enabled: bool,
}
impl<P: ProcSource, T: PaneSource> Aggregator<P, T> {
    pub fn new(proc_source: P, pane_source: T, proc_root: impl Into<PathBuf>) -> Self {
        Self {
            proc_source,
            pane_source,
            proc_root: proc_root.into(),
            cache: HashMap::new(),
            pane_map: PaneMap::default(),
            last_pane_refresh: None,
            stats: Arc::new(Stats::default()),
            attribution_enabled: true,
            panes_enabled: true,
        }
    }
    pub fn with_stats(mut self, stats: Arc<Stats>) -> Self {
        self.stats = stats;
        self
    }
    pub fn set_attribution_enabled(&mut self, enabled: bool) {
        self.attribution_enabled = enabled;
        if !enabled {
            self.cache.clear();
        }
    }
    pub fn set_panes_enabled(&mut self, enabled: bool) {
        self.panes_enabled = enabled;
        // Re-enabling must fetch fresh metadata even if the previous cadence
        // has not elapsed; the old map was intentionally discarded.
        self.last_pane_refresh = None;
        if !enabled {
            self.pane_map = PaneMap::default();
        }
    }
    pub fn update(&mut self, frame: &ConnectionsFrame) {
        self.stats.frames.fetch_add(1, Ordering::Relaxed);
        if !self.attribution_enabled {
            return;
        }
        self.refresh_pane_map_if_due();
        let now = Instant::now();
        let mut live = HashSet::new();
        let mut retry = Vec::new();
        for c in &frame.connections {
            let Some(k) = Self::key(c) else { continue };
            live.insert(k.clone());
            if k.protocol == "udp" {
                self.cache.entry(k).or_insert_with(|| {
                    Attribution::unresolved(
                        AttributionStatus::Unsupported,
                        Some("UDP process attribution is unsupported"),
                    )
                });
                continue;
            }
            // Check cached pid identity before it can be rendered again. A
            // pid directory alone is not proof: Linux may have recycled it.
            if self
                .cache
                .get(&k)
                .is_some_and(|a| !Self::cached_process_identity_is_live(&self.proc_root, a))
            {
                self.cache.insert(
                    k.clone(),
                    Attribution::unresolved(AttributionStatus::Unknown, None),
                );
            }
            if self.cache.get(&k).is_none_or(|a| {
                a.pid.is_none() && now.duration_since(a.last_attempt) >= UNRESOLVED_RETRY
            }) {
                retry.push(k);
            }
        }
        if !retry.is_empty() {
            self.stats.proc_scans.fetch_add(1, Ordering::Relaxed);
            let snap = self.proc_source.snapshot();
            for k in retry {
                let attr = self.resolve(&k, &snap);
                if attr.pid.is_none() {
                    self.stats
                        .unresolved_sockets
                        .fetch_add(1, Ordering::Relaxed);
                }
                self.cache.insert(k, attr);
            }
        }
        self.cache.retain(|k, _| live.contains(k));
    }
    fn cached_process_identity_is_live(proc_root: &std::path::Path, attr: &Attribution) -> bool {
        let Some(pid) = attr.pid else { return true };
        proc_exists(proc_root, pid)
            && attr
                .pid_start_time
                .is_none_or(|start| read_start_time(proc_root, pid) == Some(start))
    }
    fn key(c: &ConnectionEntry) -> Option<ConnectionKey> {
        Some(ConnectionKey {
            id: c.id.clone(),
            protocol: c.metadata.network.to_ascii_lowercase(),
            source: c.source_socket()?,
        })
    }
    fn resolve(&self, k: &ConnectionKey, snap: &ProcSnapshot) -> Attribution {
        if k.protocol != "tcp" {
            return Attribution::unresolved(
                AttributionStatus::Unsupported,
                Some("UDP process attribution is unsupported"),
            );
        }
        if let Some(error) = &snap.error {
            return Attribution::unresolved(
                AttributionStatus::Unknown,
                Some(&format!("proc-unavailable: {error}")),
            );
        }
        let Some(inodes) = snap.tcp_local.get(&k.source) else {
            return Attribution::unresolved(AttributionStatus::Unknown, Some("socket-not-found"));
        };
        if inodes.len() != 1 {
            return Attribution::unresolved(
                AttributionStatus::Ambiguous,
                Some("multiple local sockets share this endpoint"),
            );
        }
        let Some(owners) = snap.inode_owners.get(&inodes[0]) else {
            return Attribution::unresolved(
                AttributionStatus::Unknown,
                Some("owner-unreadable-or-gone"),
            );
        };
        if owners.len() != 1 {
            return Attribution::unresolved(
                AttributionStatus::Ambiguous,
                Some("multiple processes own this socket"),
            );
        }
        let owner = &owners[0];
        let pane = if self.panes_enabled {
            self.pane_map
                .pane_of(&self.proc_root, owner.pid)
                .map(to_pane_ref)
        } else {
            None
        };
        Attribution {
            pid: Some(owner.pid),
            pid_start_time: owner.start_time,
            comm: owner.comm.clone(),
            pane,
            status: AttributionStatus::Resolved,
            reason: None,
            last_attempt: Instant::now(),
        }
    }
    fn refresh_pane_map_if_due(&mut self) {
        if !self.panes_enabled {
            return;
        }
        if self
            .last_pane_refresh
            .is_some_and(|t| t.elapsed() < PANE_REFRESH)
        {
            return;
        }
        self.pane_map = PaneMap::from_panes(self.pane_source.panes());
        self.last_pane_refresh = Some(Instant::now());
        self.stats.pane_refreshes.fetch_add(1, Ordering::Relaxed);
        for a in self.cache.values_mut() {
            if let Some(pid) = a.pid {
                if !Self::cached_process_identity_is_live(&self.proc_root, a) {
                    *a = Attribution::unresolved(AttributionStatus::Unknown, None)
                } else {
                    a.pane = self.pane_map.pane_of(&self.proc_root, pid).map(to_pane_ref)
                }
            }
        }
    }
    pub fn rows(&self, frame: &ConnectionsFrame, sort: SortMode) -> Vec<Row> {
        let mut rows: Vec<_> = frame.connections.iter().map(|c| self.row(c)).collect();
        match sort {
            SortMode::BytesDesc => {
                rows.sort_by_key(|r| std::cmp::Reverse(r.upload.saturating_add(r.download)))
            }
            SortMode::NewestFirst => rows.sort_by_key(|r| r.age_seconds.unwrap_or(i64::MAX)),
        };
        rows
    }
    fn row(&self, c: &ConnectionEntry) -> Row {
        let fallback = if self.attribution_enabled {
            Attribution::unresolved(AttributionStatus::Unknown, None)
        } else {
            Attribution::disabled()
        };
        let a = Self::key(c)
            .and_then(|k| self.cache.get(&k))
            .cloned()
            .unwrap_or(fallback);
        Row {
            host: c.display_host().into(),
            destination_port: c.metadata.destination_port.clone(),
            network: c.metadata.network.clone(),
            process: a.comm,
            session: a.pane.as_ref().map(|p| p.session_name.clone()),
            pane_id: a.pane.as_ref().map(|p| p.pane_id.clone()),
            rule: c.rule.clone(),
            chain: c.chain_display(),
            upload: c.upload,
            download: c.download,
            age_seconds: c
                .start_time()
                .map(|t| (chrono::Utc::now() - t).num_seconds().max(0)),
            pid: a.pid,
            id: c.id.clone(),
            source_ip: c.metadata.source_ip.clone(),
            source_port: c.metadata.source_port.clone(),
            destination_ip: c.metadata.destination_ip.clone(),
            chains: c.chains.clone(),
            rule_payload: c.rule_payload.clone(),
            attribution_status: a.status.as_str().into(),
            attribution_reason: a.reason,
            pid_start_time: a.pid_start_time,
        }
    }
    pub fn stats(&self) -> &Stats {
        &self.stats
    }
    pub fn stats_snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            frames: self.stats.frames.load(Ordering::Relaxed),
            proc_scans: self.stats.proc_scans.load(Ordering::Relaxed),
            pane_refreshes: self.stats.pane_refreshes.load(Ordering::Relaxed),
            ws_reconnects: self.stats.ws_reconnects.load(Ordering::Relaxed),
            unresolved_sockets: self.stats.unresolved_sockets.load(Ordering::Relaxed),
            dropped_events: self.stats.dropped_events.load(Ordering::Relaxed),
            parse_errors: self.stats.parse_errors.load(Ordering::Relaxed),
        }
    }
    #[doc(hidden)]
    pub fn force_pane_refresh_for_test(&mut self) {
        self.last_pane_refresh = None;
        self.refresh_pane_map_if_due()
    }
    #[doc(hidden)]
    pub fn force_unresolved_retry_for_test(&mut self) {
        for attribution in self.cache.values_mut() {
            if attribution.pid.is_none() {
                attribution.last_attempt = Instant::now() - UNRESOLVED_RETRY;
            }
        }
    }
}
fn to_pane_ref(p: &PaneEntry) -> PaneRef {
    PaneRef {
        session_name: p.session_name.clone(),
        pane_id: p.pane_id.clone(),
    }
}
pub fn frame_is_useful(frame: &ConnectionsFrame) -> bool {
    !frame.connections.is_empty()
}
pub fn human_bytes(n: u64) -> String {
    const U: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024. && i < U.len() - 1 {
        v /= 1024.;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Metadata;
    use crate::procnet::ProcInfo;
    use std::cell::{Cell, RefCell};
    use std::net::{IpAddr, Ipv4Addr};

    struct FakeProc {
        snapshots: RefCell<Vec<ProcSnapshot>>,
        calls: Cell<u32>,
    }
    impl ProcSource for FakeProc {
        fn snapshot(&self) -> ProcSnapshot {
            self.calls.set(self.calls.get() + 1);
            self.snapshots.borrow_mut().remove(0)
        }
    }
    struct NoPanes;
    impl PaneSource for NoPanes {
        fn panes(&self) -> Vec<PaneEntry> {
            Vec::new()
        }
    }
    struct PanicPanes;
    impl PaneSource for PanicPanes {
        fn panes(&self) -> Vec<PaneEntry> {
            panic!("tmux source was called while panes are disabled")
        }
    }
    struct MutablePanes {
        panes: RefCell<Vec<PaneEntry>>,
        calls: Cell<u32>,
    }
    impl PaneSource for MutablePanes {
        fn panes(&self) -> Vec<PaneEntry> {
            self.calls.set(self.calls.get() + 1);
            self.panes.borrow().clone()
        }
    }
    fn c(id: &str, network: &str, port: u16) -> ConnectionEntry {
        ConnectionEntry {
            id: id.into(),
            metadata: Metadata {
                network: network.into(),
                source_ip: "127.0.0.1".into(),
                source_port: port.to_string(),
                destination_ip: "203.0.113.9".into(),
                ..Default::default()
            },
            upload: 1,
            download: 2,
            ..Default::default()
        }
    }
    fn frame(cs: Vec<ConnectionEntry>) -> ConnectionsFrame {
        ConnectionsFrame {
            upload_total: 0,
            download_total: 0,
            connections: cs,
            received_at: None,
        }
    }
    fn socket(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }
    fn fixture_root(pid: u32, start: u64) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mihomo-top-state-{}-{pid}-{start}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(pid.to_string())).unwrap();
        write_stat(&root, pid, start);
        root
    }
    fn write_stat(root: &std::path::Path, pid: u32, start: u64) {
        let mut fields = vec!["0".to_string(); 20];
        fields[0] = "S".into();
        fields[1] = "1".into();
        fields[19] = start.to_string();
        std::fs::write(
            root.join(pid.to_string()).join("stat"),
            format!("{pid} (fixture) {}\n", fields.join(" ")),
        )
        .unwrap();
    }

    #[test]
    fn cache_identity_prevents_stale_reuse_and_retries_unknown() {
        let root = fixture_root(12, 77);
        let empty = ProcSnapshot::default();
        let mut first = ProcSnapshot::default();
        first.tcp_local.insert(socket(9000), vec![7]);
        first.inode_owners.insert(
            7,
            vec![ProcInfo {
                pid: 12,
                comm: "first".into(),
                start_time: Some(77),
            }],
        );
        let mut third = ProcSnapshot::default();
        third.tcp_local.insert(socket(9000), vec![9]);
        third.inode_owners.insert(
            9,
            vec![ProcInfo {
                pid: 14,
                comm: "third".into(),
                start_time: None,
            }],
        );
        let proc = FakeProc {
            snapshots: RefCell::new(vec![empty, first, third]),
            calls: Cell::new(0),
        };
        let mut a = Aggregator::new(proc, NoPanes, root.clone());
        let one = frame(vec![c("core-a", "tcp", 9000)]);
        a.update(&one);
        assert!(a.rows(&one, SortMode::BytesDesc)[0].process.is_empty());
        a.force_unresolved_retry_for_test();
        a.update(&one);
        assert_eq!(a.rows(&one, SortMode::BytesDesc)[0].process, "first");
        let two = frame(vec![c("core-b", "tcp", 9000)]);
        a.update(&two);
        assert_eq!(a.rows(&two, SortMode::BytesDesc)[0].process, "third");
        assert_eq!(a.stats_snapshot().proc_scans, 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pane_metadata_refreshes_when_session_is_renamed() {
        let root = fixture_root(22, 77);
        let mut snap = ProcSnapshot::default();
        snap.tcp_local.insert(socket(9100), vec![7]);
        snap.inode_owners.insert(
            7,
            vec![ProcInfo {
                pid: 22,
                comm: "client".into(),
                start_time: Some(77),
            }],
        );
        let panes = MutablePanes {
            panes: RefCell::new(vec![PaneEntry {
                pane_id: "%1".into(),
                pane_pid: 22,
                session_name: "before".into(),
            }]),
            calls: Cell::new(0),
        };
        let mut a = Aggregator::new(
            FakeProc {
                snapshots: RefCell::new(vec![snap]),
                calls: Cell::new(0),
            },
            panes,
            root.clone(),
        );
        let f = frame(vec![c("rename", "tcp", 9100)]);
        a.update(&f);
        assert_eq!(
            a.rows(&f, SortMode::BytesDesc)[0].session.as_deref(),
            Some("before")
        );
        a.pane_source.panes.replace(vec![PaneEntry {
            pane_id: "%1".into(),
            pane_pid: 22,
            session_name: "after".into(),
        }]);
        a.force_pane_refresh_for_test();
        assert_eq!(
            a.rows(&f, SortMode::BytesDesc)[0].session.as_deref(),
            Some("after")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn no_tmux_still_rejects_reused_pid_by_start_time() {
        let root = fixture_root(23, 100);
        let mut snap = ProcSnapshot::default();
        snap.tcp_local.insert(socket(9101), vec![8]);
        snap.inode_owners.insert(
            8,
            vec![ProcInfo {
                pid: 23,
                comm: "old-process".into(),
                start_time: Some(100),
            }],
        );
        let mut a = Aggregator::new(
            FakeProc {
                snapshots: RefCell::new(vec![snap]),
                calls: Cell::new(0),
            },
            PanicPanes,
            root.clone(),
        );
        a.set_panes_enabled(false);
        let f = frame(vec![c("reused", "tcp", 9101)]);
        a.update(&f);
        assert_eq!(a.rows(&f, SortMode::BytesDesc)[0].process, "old-process");
        write_stat(&root, 23, 101);
        a.update(&f);
        let row = &a.rows(&f, SortMode::BytesDesc)[0];
        assert_eq!(row.attribution_status, "unknown");
        assert!(row.process.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn udp_is_explicitly_unsupported_and_tcp_ambiguity_is_not_guessed() {
        let mut snap = ProcSnapshot::default();
        snap.tcp_local.insert(socket(9001), vec![1, 2]);
        snap.tcp_local.insert(socket(9002), vec![3]);
        snap.inode_owners.insert(
            3,
            vec![
                ProcInfo {
                    pid: 30,
                    comm: "one".into(),
                    start_time: None,
                },
                ProcInfo {
                    pid: 31,
                    comm: "two".into(),
                    start_time: None,
                },
            ],
        );
        let proc = FakeProc {
            snapshots: RefCell::new(vec![snap]),
            calls: Cell::new(0),
        };
        let mut a = Aggregator::new(proc, NoPanes, "/definitely-missing");
        let f = frame(vec![
            c("u", "udp", 9001),
            c("t", "tcp", 9001),
            c("shared", "tcp", 9002),
        ]);
        a.update(&f);
        let rows = a.rows(&f, SortMode::BytesDesc);
        assert_eq!(
            rows.iter()
                .find(|r| r.id == "u")
                .unwrap()
                .attribution_status,
            "unsupported"
        );
        let tcp = rows.iter().find(|r| r.id == "t").unwrap();
        assert_eq!(tcp.attribution_status, "ambiguous");
        assert!(tcp.process.is_empty());
        assert_eq!(
            rows.iter()
                .find(|r| r.id == "shared")
                .unwrap()
                .attribution_status,
            "ambiguous"
        );
    }

    #[test]
    fn two_distinct_pids_for_one_inode_are_ambiguous() {
        let mut snap = ProcSnapshot::default();
        snap.tcp_local.insert(socket(9003), vec![4]);
        snap.inode_owners.insert(
            4,
            vec![
                ProcInfo {
                    pid: 41,
                    comm: "one".into(),
                    start_time: None,
                },
                ProcInfo {
                    pid: 42,
                    comm: "two".into(),
                    start_time: None,
                },
            ],
        );
        let mut a = Aggregator::new(
            FakeProc {
                snapshots: RefCell::new(vec![snap]),
                calls: Cell::new(0),
            },
            NoPanes,
            "/missing",
        );
        let f = frame(vec![c("two-pids", "tcp", 9003)]);
        a.update(&f);
        let row = &a.rows(&f, SortMode::BytesDesc)[0];
        assert_eq!(row.attribution_status, "ambiguous");
        assert_eq!(
            row.attribution_reason.as_deref(),
            Some("multiple processes own this socket")
        );
        assert!(row.process.is_empty());
    }

    #[test]
    fn byte_sort_is_overflow_safe_and_newest_is_smallest_age() {
        let a = Aggregator::new(
            FakeProc {
                snapshots: RefCell::new(vec![]),
                calls: Cell::new(0),
            },
            NoPanes,
            "/missing",
        );
        let mut x = c("huge", "udp", 1);
        x.upload = u64::MAX;
        x.download = 10;
        let mut y = c("small", "udp", 2);
        y.upload = 100;
        y.download = 0;
        let f = frame(vec![y, x]);
        assert_eq!(a.rows(&f, SortMode::BytesDesc)[0].id, "huge");
    }
}
