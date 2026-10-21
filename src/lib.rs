//! Read-only Linux monitoring for mihomo's `/connections` WebSocket stream.
//!
//! The core supplies connection facts. Optional same-host `/proc` reads and
//! tmux pane queries add best-effort attribution without requiring root.
//! Snapshots can miss short connections and do not constitute traffic history.
//!
//! The binary runs one socket reader and one aggregation worker. The main
//! thread renders the TUI or emits JSON snapshots. The aggregator is the sole
//! Dashboard writer; statistics are shared atomic counters. A bounded event
//! channel limits queued snapshots when consumers fall behind.

#![forbid(unsafe_code)]
#[cfg(not(target_os = "linux"))]
compile_error!("mihomo-top currently supports Linux only");

pub mod config;
pub mod error;
pub mod model;
pub mod output;
pub mod panemap;
pub mod procnet;
pub mod runtime;
pub mod state;
pub mod ui;
pub mod ws;
