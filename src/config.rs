//! Command-line configuration. Controller credentials are never accepted in argv.

use std::fs::OpenOptions;
use std::io::Read;
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{FlowError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Auto,
    Tui,
    Once,
    Stream,
}

#[derive(Clone)]
pub struct Config {
    pub controller: String,
    pub tick_ms: u64,
    pub timeout_secs: u64,
    pub secret: Option<String>,
    pub tmux_socket: Option<String>,
    pub mode: RunMode,
    pub attribution: bool,
    pub panes: bool,
    /// `--once` exits non-zero when the controller never delivers a frame.
    pub strict: bool,
    /// Include the raw controller error after the short warning.
    pub verbose: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            controller: "127.0.0.1:9090".into(),
            tick_ms: 1000,
            timeout_secs: 10,
            secret: None,
            tmux_socket: None,
            mode: RunMode::Auto,
            attribution: true,
            panes: true,
            strict: false,
            verbose: false,
        }
    }
}

impl Config {
    pub fn from_args(argv: &[String]) -> Result<Self> {
        let mut cfg = Self::default();
        let mut secret_file: Option<PathBuf> = None;
        let mut format = None;
        let mut args = argv.iter();
        while let Some(arg) = args.next() {
            let mut value = || {
                args.next()
                    .cloned()
                    .ok_or_else(|| FlowError::Config(format!("{arg}: missing value")))
            };
            match arg.as_str() {
                "--controller" => cfg.controller = value()?,
                "--socket" => cfg.tmux_socket = Some(value()?),
                "--secret-file" => secret_file = Some(PathBuf::from(value()?)),
                "--tick-ms" => {
                    cfg.tick_ms = value()?
                        .parse()
                        .map_err(|_| FlowError::Config("--tick-ms expects an integer".into()))?;
                }
                "--timeout-secs" => {
                    cfg.timeout_secs = value()?.parse().map_err(|_| {
                        FlowError::Config("--timeout-secs expects an integer".into())
                    })?;
                }
                "--format" => format = Some(value()?),
                "--no-attribution" => cfg.attribution = false,
                "--no-tmux" => cfg.panes = false,
                "--strict" => cfg.strict = true,
                "--verbose" => cfg.verbose = true,
                "tui" | "--once" | "--stream" => {
                    if cfg.mode != RunMode::Auto {
                        return Err(FlowError::Config("choose exactly one output mode".into()));
                    }
                    cfg.mode = match arg.as_str() {
                        "tui" => RunMode::Tui,
                        "--once" => RunMode::Once,
                        _ => RunMode::Stream,
                    };
                }
                _ => return Err(FlowError::Config(format!("unknown argument: {arg}"))),
            }
        }
        if !(100..=60_000).contains(&cfg.tick_ms) {
            return Err(FlowError::Config("--tick-ms must be 100..60000".into()));
        }
        if !(1..=3600).contains(&cfg.timeout_secs) {
            return Err(FlowError::Config("--timeout-secs must be 1..3600".into()));
        }
        let address: SocketAddr = cfg.controller.parse().map_err(|_| {
            FlowError::Config(
                "--controller expects a numeric loopback HOST:PORT (IPv6: [::1]:9090)".into(),
            )
        })?;
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(FlowError::Config(
                "--controller must use a nonzero loopback port; remote attribution is unsupported"
                    .into(),
            ));
        }
        cfg.controller = address.to_string();
        if let Some(format) = format {
            let requested = match format.as_str() {
                "json" => RunMode::Once,
                "jsonl" => RunMode::Stream,
                _ => return Err(FlowError::Config("--format expects json or jsonl".into())),
            };
            if cfg.mode != RunMode::Auto && cfg.mode != requested {
                return Err(FlowError::Config(
                    "--format conflicts with the selected mode".into(),
                ));
            }
            cfg.mode = requested;
        }
        cfg.secret = resolve_secret(secret_file.as_deref(), std::env::var("MIHOMO_SECRET").ok())?;
        Ok(cfg)
    }

    pub fn connections_url(&self) -> String {
        format!("ws://{}/connections", self.controller)
    }

    pub fn usage() -> &'static str {
        "mihomo-top — read-only Linux connection monitor for mihomo\n\n\
         USAGE:\n\
           mihomo-top [tui | --once | --stream] [OPTIONS]\n\n\
         MODES:\n\
           tui                  Interactive terminal view\n\
           --once               One JSON snapshot, then exit\n\
           --stream             Continuous JSONL snapshots and state heartbeats\n\
           Default              TUI when stdin/stdout are terminals; stream otherwise\n\n\
         OPTIONS:\n\
           --controller ADDR    Numeric loopback HOST:PORT [127.0.0.1:9090]\n\
           --socket PATH        Explicit tmux socket; otherwise current/default server\n\
           --secret-file PATH   Regular, non-symlink file with mode 0600\n\
           --no-attribution     Skip local process and pane attribution\n\
           --no-tmux            Skip tmux pane queries\n\
           --tick-ms N          Output/UI interval, 100..60000 [1000]\n\
           --timeout-secs N     First-frame deadline for --once, 1..3600 [10]\n\
           --strict             --once exits non-zero if the controller is unreachable\n\
           --verbose            After the offline warning, also print the raw error\n\
           --format json|jsonl  Select --once or --stream format\n\
           -h, --help           Show this help\n\
           -V, --version        Show version\n\n\
         Secret fallback: MIHOMO_SECRET. Never pass the secret as an argument.\n\
         Scope: connections traversing mihomo only; local UDP attribution unsupported.\n"
    }
}

fn resolve_secret(path: Option<&Path>, env_secret: Option<String>) -> Result<Option<String>> {
    let secret = if let Some(path) = path {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| FlowError::Config(format!("--secret-file {}: {e}", path.display())))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o7777 != 0o600 {
            return Err(FlowError::Config(
                "--secret-file must be a regular file with mode 0600".into(),
            ));
        }
        let mut value = String::new();
        (&mut file).take(8193).read_to_string(&mut value)?;
        if value.len() > 8192 {
            return Err(FlowError::Config(
                "controller secret exceeds 8192 bytes".into(),
            ));
        }
        Some(value.trim().to_string())
    } else {
        env_secret
    };
    match secret.filter(|value| !value.is_empty()) {
        Some(value) if value.len() > 8192 || value.chars().any(char::is_control) => {
            Err(FlowError::Config(
                "controller secret contains invalid characters or is too long".into(),
            ))
        }
        value => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Config> {
        Config::from_args(&args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn modes_and_formats_are_consistent() {
        assert_eq!(parse(&["--once"]).unwrap().mode, RunMode::Once);
        assert_eq!(parse(&["--format", "jsonl"]).unwrap().mode, RunMode::Stream);
        assert!(parse(&["tui", "--format", "json"]).is_err());
        assert!(parse(&["--once", "--stream"]).is_err());
        assert!(parse(&["--strict", "--verbose"]).unwrap().strict);
        assert!(parse(&["--verbose"]).unwrap().verbose);
        assert!(!parse(&["--once"]).unwrap().strict);
    }

    #[test]
    fn controller_cannot_point_to_another_host() {
        assert!(parse(&["--controller", "192.0.2.1:9090"]).is_err());
        assert!(parse(&["--controller", "localhost:9090"]).is_err());
        assert_eq!(
            parse(&["--controller", "[::1]:9090"])
                .unwrap()
                .connections_url(),
            "ws://[::1]:9090/connections"
        );
    }

    #[test]
    fn invalid_numeric_limits_are_rejected() {
        for args in [
            ["--tick-ms", "0"],
            ["--tick-ms", "18446744073709551615"],
            ["--timeout-secs", "0"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn secret_resolution_does_not_modify_process_environment() {
        assert_eq!(
            resolve_secret(None, Some("synthetic".into()))
                .unwrap()
                .as_deref(),
            Some("synthetic")
        );
        assert!(resolve_secret(None, Some("synthetic\nheader".into())).is_err());
    }

    #[test]
    fn secret_file_rejects_mode_and_symlink() {
        let root = std::env::temp_dir().join(format!("mihomo-top-secret-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("secret");
        std::fs::write(&file, "synthetic\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(resolve_secret(Some(&file), None).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            resolve_secret(Some(&file), None).unwrap().as_deref(),
            Some("synthetic")
        );
        let link = root.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(resolve_secret(Some(&link), None).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
