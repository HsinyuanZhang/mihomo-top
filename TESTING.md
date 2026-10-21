# Testing

`mihomo-top` reads the local mihomo controller's connection stream and, when it can, attributes each TCP socket to a process and a tmux pane. It does **not** inspect `HTTP_PROXY`, `HTTPS_PROXY`, or `ALL_PROXY`, it does not query an IP-echo service, and it does not detect TUN mode. Direct connections that never enter mihomo are absent from the table. The lab below is how those facts are checked.

## Offline tests

From the repository root, with Rust 1.88 or newer:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

`cargo test` does not start mihomo. The ignored tests in `tests/live_controller.rs` are the live lab; run them with `scripts/test-env.sh`.

## Live lab

```sh
scripts/test-env.sh
```

On Linux this command:

1. Downloads mihomo **v1.19.32** (`mihomo-linux-amd64`) into `.cache/mihomo/` and checks its SHA-256.
2. Starts a local mock that answers `{"ip":"203.0.113.10","ok":true}` and a local HTTP upstream that accepts `CONNECT`.
3. Starts mihomo with `external-controller` on `127.0.0.1`, a synthetic secret, `tun.enable: false`, one `DIRECT` rule, and one rule that sends a single destination port through `mock-upstream`. There is no subscription and no outbound account.
4. Spawns processes with different environments and runs `mihomo-top --once` (and a short TUI capture) against them.

| Process | Environment | What the lab asserts |
| --- | --- | --- |
| `mht-none` | no proxy variables | Connectivity succeeds and the fake egress IP is `203.0.113.10`. The snapshot has **no** row for this pid. |
| `mht-http` | `HTTP_PROXY` / `HTTPS_PROXY` → mihomo's HTTP port | Row is visible, chain `mock-upstream`, attribution `resolved`, destination `127.0.0.1` and the mock port. |
| `mht-socks` | `ALL_PROXY=socks5://…` inside a private tmux server | Same chain, plus session `mhtlab` and a pane id. tmux 3.5 prints the field separator as `\037`; the parser accepts that and the older raw separator. |
| `mht-direct` | `HTTP_PROXY` → mihomo, target matches `MATCH,DIRECT` | Row is visible with chain `DIRECT`. |
| `mht-bad` | `HTTP_PROXY` → `127.0.0.1:1` | The client fails. No row. |
| controller stopped | mihomo killed | `--once` exits 0. stderr is `WARNING: Mihomo offline (127.0.0.1:PORT unreachable) — processes are NOT proxied`, with no raw socket error and no secret. stdout is a snapshot whose `connection_state` is `offline` and whose rows (if any) have `proxy_status` `mihomo-offline`. `--strict` exits non-zero and writes no snapshot. A client aimed at the old HTTP port fails. |
| controller recovers | TUI starts while mihomo is stopped, then mihomo starts | The TUI shows the yellow warning and lists the pane process as `mihomo-offline`. After mihomo is up, the warning clears and a held client appears as a normal connection. |

The report columns are:

- **status** — monitor `connection_state`, and for each process either `not-in-mihomo` or `visible <chain> <attribution>`.
- **egress_ip** — the address the mock put in the HTTP body (`203.0.113.10`), not a field mihomo-top prints.
- **reported_ip** — `destination_ip` from the controller snapshot. For this lab that is `127.0.0.1`.
- **connectivity** — whether the client actually received the mock response.

An idle controller (mihomo up, no clients) is asserted separately: `connection_state` is `connected` and `connection_count` is 0. That is different from a stopped controller.

### Optional live network

```sh
scripts/test-env.sh live-network
```

This sets `MIHOMO_TOP_LIVE_NETWORK=1`. One client holds a SOCKS connection to `example.com:80` through mihomo `DIRECT` and the test checks that the row's destination matches that address. Another client requests `http://ifconfig.me/ip` and the test checks that the body is a public IPv4 address. Artifacts redact that address. The default `scripts/test-env.sh` does not do this.

### Platform check

```sh
scripts/test-env.sh check
```

Exits 0 on Linux and 2 on any other OS. macOS CI uses that exit code, then runs `cargo check` and expects the crate's Linux-only `compile_error!`.

## What this does not test

- **TUN mode.** The lab config sets `tun.enable: false`. A separate probe may open `/dev/net/tun` and create a transient interface, then close it so nothing is routed. mihomo is never started with TUN, because that would capture host traffic and because v0.1 has no TUN column: traffic would only appear if the controller reported the resulting connections.
- **macOS behavior of the monitor.** The crate does not compile off Linux. Pane and process attribution read Linux `/proc`.
- **UDP attribution.** v0.1 reports UDP rows as `unsupported` and does not map them to a process. The offline unit tests cover that; the live lab uses TCP.
- **Real subscriptions, remote controllers, or provider credentials.** The controller secret in the lab is the fixed string `lab-secret-not-a-credential`.

## CI

`.github/workflows/ci.yml` runs the offline fmt/clippy/test/MSRV jobs on Ubuntu, the live lab on Ubuntu, and the non-Linux guard on macOS.

## Layout

| Path | Role |
| --- | --- |
| `scripts/test-env.sh` | Download mihomo and run the suite |
| `scripts/lab.py` | Mock endpoint, upstream proxy, scenario clients |
| `tests/scenario_report.rs` | Unit tests for the status / IP / connectivity report |
| `tests/controller_fixture.rs` | Sanitized mihomo v1.19.32 `/connections` payload |
| `tests/live_controller.rs` | Ignored integration tests against the real binary |
