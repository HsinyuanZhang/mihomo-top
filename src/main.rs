//! CLI entry point. Workers are joined and the terminal restored on scope exit.

#![forbid(unsafe_code)]

use std::io::{self, IsTerminal};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::{backend::CrosstermBackend, Terminal};

use mihomo_top::config::{Config, RunMode};
use mihomo_top::output::{write_snapshot, CancellableStdout};
use mihomo_top::runtime::Runtime;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{}", Config::usage());
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("mihomo-top {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let config = match Config::from_args(&args) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("mihomo-top: {error}\nRun mihomo-top --help for usage.");
            return ExitCode::from(2);
        }
    };
    match run(&config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error)
            if error.chain().any(|cause| {
                cause
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
            }) =>
        {
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("mihomo-top: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(config: &Config) -> Result<()> {
    let mode = match config.mode {
        RunMode::Auto if io::stdin().is_terminal() && io::stdout().is_terminal() => RunMode::Tui,
        RunMode::Auto => RunMode::Stream,
        mode => mode,
    };
    if mode == RunMode::Tui {
        return run_tui(config);
    }
    let runtime = Runtime::start(config)?;
    let mut output = CancellableStdout::new(std::sync::Arc::clone(&runtime.stop))?;
    let started = Instant::now();
    let mut last_output = Instant::now() - Duration::from_millis(config.tick_ms);
    while !runtime.stop.load(Ordering::Relaxed) {
        if runtime.worker_failed() {
            anyhow::bail!("monitor worker stopped unexpectedly");
        }
        let dashboard = runtime.snapshot()?;
        if mode == RunMode::Once {
            if dashboard.last_frame_at.is_some() {
                if let Err(error) = write_snapshot(&mut output, &dashboard, true) {
                    if runtime.stop.load(Ordering::Relaxed)
                        && error.kind() == io::ErrorKind::Interrupted
                    {
                        return Ok(());
                    }
                    return Err(error).context("write JSON snapshot");
                }
                return Ok(());
            }
            if started.elapsed() >= Duration::from_secs(config.timeout_secs) {
                anyhow::bail!(
                    "no valid frame within {} s: {}",
                    config.timeout_secs,
                    dashboard
                        .last_error
                        .as_deref()
                        .unwrap_or("controller did not deliver a snapshot")
                );
            }
        } else if last_output.elapsed() >= Duration::from_millis(config.tick_ms) {
            if let Err(error) = write_snapshot(&mut output, &dashboard, false) {
                if runtime.stop.load(Ordering::Relaxed)
                    && error.kind() == io::ErrorKind::Interrupted
                {
                    return Ok(());
                }
                return Err(error).context("write JSONL snapshot");
            }
            last_output = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        let guard = Self;
        crossterm::execute!(io::stdout(), EnterAlternateScreen)
            .context("enter alternate screen")?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

fn run_tui(config: &Config) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        anyhow::bail!("tui requires terminal stdin and stdout; use --stream or --once");
    }
    mihomo_top::error::set_quiet(true);
    // Register signals before entering raw mode. Restore the terminal before
    // joining workers, so even a slow local scan does not hold the terminal.
    let runtime = Runtime::start(config)?;
    let _guard = TerminalGuard::enter()?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("terminal backend")?;
    let mut ui = mihomo_top::ui::UiState::new();
    let mut last_draw = Instant::now() - Duration::from_millis(config.tick_ms);
    while !runtime.stop.load(Ordering::Relaxed) {
        if runtime.worker_failed() {
            anyhow::bail!("monitor worker stopped unexpectedly");
        }
        if crossterm::event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        KeyCode::Char('s' | 'S') => {
                            runtime.sort.fetch_xor(1, Ordering::Relaxed);
                        }
                        KeyCode::Down => ui.table.select_next(),
                        KeyCode::Up => ui.table.select_previous(),
                        _ => {}
                    }
                }
            }
        }
        if last_draw.elapsed() >= Duration::from_millis(config.tick_ms) {
            let dashboard = runtime.snapshot()?;
            terminal.draw(|frame| mihomo_top::ui::draw(frame, &dashboard, &mut ui))?;
            last_draw = Instant::now();
        }
    }
    Ok(())
}
