# Validation

Run these commands from the repository root after installing the required Rust toolchains. They use the locked dependency graph and do not require a live mihomo controller.

| Check | Command | Current reported result |
| --- | --- | --- |
| Formatting | `cargo +stable fmt --all --check` | Passed |
| Linting | `cargo +stable clippy --locked --all-targets -- -D warnings` | Passed |
| Stable tests | `cargo +stable test --locked` | Passed |
| MSRV tests | `cargo +1.88.0 test --locked` | Passed |
| Stable release build | `cargo +stable build --release --locked` | Passed |
| MSRV release build | `cargo +1.88.0 build --release --locked` | Passed |

The MSRV is Rust 1.88. At the latest validation, both stable and MSRV test runs passed 53 unit tests and 6 CLI tests. These counts are evidence from that run, not a public compatibility contract. Re-run the matrix after code, dependency, or toolchain changes and record only results from that run.

For manual output checks, start a local mihomo controller in a safe test environment, then verify that `--once` produces one valid JSON document and `--stream` produces newline-delimited complete snapshots. When testing interruption of a blocked pipe, accept an unterminated final JSONL line and ensure the consumer ignores it.

The repeatable version of that check is `scripts/test-env.sh`. It installs pinned mihomo v1.19.32, serves a documentation egress address from a local mock, and runs the ignored live tests. See [TESTING.md](../TESTING.md). Those live tests are not part of the offline `cargo test` count above.

## Compatibility observations

These are observed test combinations, not a compatibility promise.

| Component | Observed combination | Result |
| --- | --- | --- |
| Linux platform | x86_64, Ubuntu 22.04, kernel 6.8 | Validated |
| Rust | 1.88 and current stable | MSRV tests passed; stable checks passed |
| mihomo controller | 1.19.25 | Live `--once` snapshot check validated; rows received and local process/pane attribution resolved in the test session |
| tmux | 3.2a | Optional attribution path and independent TUI status, sorting, and quit controls validated |
| tmux | 3.5a | `list-panes -F` octal-escapes the unit separator; pane parsing covers that form |
| mihomo controller | 1.19.32, loopback lab, no subscription | `scripts/test-env.sh` on Linux amd64 |
| Other Linux distributions, mihomo versions, and ARM | — | Not yet tested |
