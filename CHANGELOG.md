# Changelog

All notable user-facing changes are recorded here.

## Unreleased

### Changed
- An unreachable controller no longer ends the TUI or `--once`. The screen
  shows `WARNING: Mihomo offline (HOST:PORT unreachable) — processes are NOT proxied`,
  keeps retrying, and lists known processes as `mihomo-offline` until mihomo
  returns. `--once` prints that warning and a snapshot, then exits 0.
  `--strict` restores the non-zero `--once` exit. `--verbose` adds the raw
  socket error.

### Fixed
- Pane attribution on tmux 3.5. That version prints the unit separator in
  `list-panes -F` as the octal escape `\037`, so the pane list was parsed as
  empty and every connection stayed without a session.

### Added
- Reproducible Linux lab (`scripts/test-env.sh`): a pinned mihomo with a
  loopback DIRECT outbound and a mock upstream, fake egress responses, and
  live tests for process proxy settings, attribution, and a stopped
  controller. GitHub Actions runs that lab on Linux and checks the Linux-only
  build guard on macOS.
- TUI polish (no new dependencies): semantic colors (connection state
  green/yellow/red, DIRECT chains dimmed, proxied chains accented,
  unresolved-attribution reasons in yellow, byte magnitudes tinted at
  MiB/GiB), a per-connection share bar (█ blocks relative to the busiest
  row), direction arrow on the active sort column, right-aligned numeric
  columns, bold selected row.
- `scripts/install.sh`: single `mihomo-top` launcher in ~/.local/bin
  (install/remove); replaces the earlier per-repo `netmon`/`flow` wrappers.
  The tmux-netmon repository's Rust monitor (mihomo-flow) is retired in
  favor of this crate as the single maintained implementation.

- Experimental v0.1 Linux monitor for live mihomo controller connections.
- Terminal dashboard, one-frame JSON, and JSONL streaming interfaces.
- Optional best-effort TCP process and tmux-pane attribution.
- Schema version 1 output contract and documentation of known scope limits.
