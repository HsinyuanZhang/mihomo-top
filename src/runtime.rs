//! Shared worker lifecycle. Only the aggregator writes the dashboard.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use crate::config::Config;
use crate::model::ConnectionsFrame;
use crate::state::{
    is_offline_reason, mark_rows_offline, offline_warning, Aggregator, Dashboard, RealPaneSource,
    RealProcSource, SortMode, Stats,
};
use crate::ws::{self, WsEvent};

pub struct Runtime {
    pub stop: Arc<AtomicBool>,
    pub sort: Arc<AtomicU8>,
    dashboard: Arc<RwLock<Dashboard>>,
    workers: Vec<JoinHandle<()>>,
    signals: Vec<signal_hook::SigId>,
}

impl Runtime {
    pub fn start(cfg: &Config) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut signals = Vec::new();
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            match signal_hook::flag::register(signal, Arc::clone(&stop)) {
                Ok(id) => signals.push(id),
                Err(error) => {
                    for id in signals {
                        signal_hook::low_level::unregister(id);
                    }
                    return Err(error.into());
                }
            }
        }
        let sort = Arc::new(AtomicU8::new(0));
        let dashboard = Arc::new(RwLock::new(Dashboard {
            controller: cfg.controller.clone(),
            secret_in_use: cfg.secret.is_some(),
            connection_state: "connecting".into(),
            stale: true,
            local_attribution: cfg.attribution,
            ..Dashboard::default()
        }));
        let stats = Arc::new(Stats::default());
        let (tx, rx) = mpsc::sync_channel(8);
        let reader = ws::spawn(
            cfg.connections_url(),
            cfg.secret.clone(),
            Arc::clone(&stop),
            tx,
            Arc::clone(&stats),
            cfg.verbose,
        );
        let stop_agg = Arc::clone(&stop);
        let dash_out = Arc::clone(&dashboard);
        let sort_in = Arc::clone(&sort);
        let config = cfg.clone();
        let worker = std::thread::Builder::new().name("aggregator".into()).spawn(move || {
            let mut agg = Aggregator::new(RealProcSource::new("/proc"), RealPaneSource::new(config.tmux_socket.clone(), "tmux"), "/proc").with_stats(Arc::clone(&stats));
            agg.set_attribution_enabled(config.attribution);
            agg.set_panes_enabled(config.panes);
            let mut current = Dashboard {
                controller: config.controller.clone(),
                secret_in_use: config.secret.is_some(),
                connection_state: "connecting".into(),
                stale: true,
                local_attribution: config.attribution,
                attribution_notes: vec![
                    "local process attribution is best-effort; unreadable owners are not assumed to belong to another UID".into(),
                    "UDP process attribution is unsupported; TCP ambiguities remain unknown".into(),
                    "only connections traversing mihomo are visible; snapshots can miss short connections".into(),
                ],
                ..Dashboard::default()
            };
            let mut last_frame: Option<Box<ConnectionsFrame>> = None;
            let mut last_received: Option<Instant> = None;
            while !stop_agg.load(Ordering::Relaxed) {
                let first = match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(event) => Some(event),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        current.connection_state = "stopped".into();
                        current.last_error = Some("WebSocket reader stopped".into());
                        current.stale = true;
                        if let Ok(mut output) = dash_out.write() { *output = current.clone(); }
                        break;
                    }
                };
                let mut pending = None;
                for event in first.into_iter().chain(rx.try_iter()) {
                    match event {
                        WsEvent::Frame(frame) => {
                            if pending.replace(frame).is_some() { stats.dropped_events.fetch_add(1, Ordering::Relaxed); }
                            current.connection_state = "connected".into();
                            current.last_error = None;
                            current.warning = None;
                            last_received = Some(Instant::now());
                        }
                        WsEvent::Connected => {
                            current.connection_state = "connected".into();
                            current.last_error = None;
                            current.warning = None;
                        }
                        WsEvent::Disconnected(reason) => {
                            if is_offline_reason(&reason) {
                                current.connection_state = "offline".into();
                                current.warning = Some(offline_warning(&current.controller));
                                current.last_error = if config.verbose { Some(reason) } else { None };
                            } else {
                                current.connection_state = "reconnecting".into();
                                current.warning = None;
                                current.last_error = Some(reason);
                            }
                        }
                        WsEvent::ParseError(reason) => {
                            current.connection_state = "parse_error".into();
                            current.last_error = Some(reason);
                        }
                    }
                }
                if let Some(frame) = pending {
                    agg.update(&frame);
                    current.upload_total = frame.upload_total;
                    current.download_total = frame.download_total;
                    current.connection_count = frame.connections.len();
                    current.last_frame_at = frame.received_at.map(|t| t.to_rfc3339());
                    last_frame = Some(frame);
                }
                current.sort_mode = if sort_in.load(Ordering::Relaxed) == 1 { SortMode::NewestFirst } else { SortMode::BytesDesc };
                if current.connection_state == "offline" {
                    let panes = if config.attribution && config.panes {
                        agg.offline_panes()
                    } else {
                        Vec::new()
                    };
                    if let Some(frame) = &last_frame {
                        current.rows = agg.rows(frame, current.sort_mode);
                        mark_rows_offline(&mut current.rows);
                    } else {
                        current.rows.clear();
                    }
                    for pane in panes {
                        let listed = pane.pid.is_some_and(|pid| {
                            current.rows.iter().any(|row| row.pid == Some(pid))
                        });
                        if !listed {
                            current.rows.push(pane);
                        }
                    }
                    current.connection_count = current.rows.len();
                } else if let Some(frame) = &last_frame {
                    current.rows = agg.rows(frame, current.sort_mode);
                }
                let too_old = last_received.is_none_or(|t| t.elapsed() > Duration::from_secs(3));
                let queued_too_old = last_frame.as_ref().and_then(|f| f.received_at).is_some_and(|t| (chrono::Utc::now() - t).num_milliseconds() > 3000);
                current.stale = current.connection_state != "connected" || too_old || queued_too_old;
                current.stats_snapshot = agg.stats_snapshot();
                match dash_out.write() {
                    Ok(mut output) => *output = current.clone(),
                    Err(_) => { stop_agg.store(true, Ordering::Relaxed); break; }
                }
            }
        });
        match worker {
            Ok(aggregator) => Ok(Self {
                stop,
                sort,
                dashboard,
                workers: vec![reader, aggregator],
                signals,
            }),
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                let _ = reader.join();
                for id in signals {
                    signal_hook::low_level::unregister(id);
                }
                Err(error.into())
            }
        }
    }

    pub fn snapshot(&self) -> Result<Dashboard> {
        self.dashboard
            .read()
            .map(|dashboard| dashboard.clone())
            .map_err(|_| anyhow!("dashboard lock poisoned"))
    }

    pub fn worker_failed(&self) -> bool {
        self.workers.iter().any(JoinHandle::is_finished) && !self.stop.load(Ordering::Relaxed)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        for id in self.signals.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}
