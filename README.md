# mihomo-top

`mihomo-top` is an experimental, independent Linux terminal monitor for the [mihomo](https://github.com/MetaCubeX/mihomo) controller's connection stream. It shows traffic currently traversing mihomo, with optional best-effort local process and tmux-pane attribution. It is not affiliated with or endorsed by mihomo.

It runs entirely as an ordinary user: no root access, daemon, packet capture, or kernel module is required.

> **Experimental v0.1.** Treat the output as a live observability aid, not an accounting record or a complete inventory of machine traffic. Read the [limitations](docs/limitations.md) before depending on it.

[简体中文说明](README.zh-CN.md) · [Output schema](docs/output-schema.md) · [Validation](docs/validation.md) · [Testing](TESTING.md) · [Roadmap](docs/roadmap.md) · [Contributing](CONTRIBUTING.md)

## What it monitors

The program consumes mihomo controller connection data. Its scope is limited to connections currently known to that controller, so direct traffic and traffic from other proxies are outside the view. For each visible connection it can display the destination, network type, rule and outbound chain, byte counters, age, and—when it can establish the association—the local process and tmux pane.

TCP attribution is deliberately conservative and best-effort: it resolves only a uniquely visible local socket and owner. If the visible data is shared or cannot be identified unambiguously, the result is `UNKNOWN`. Unreadable or otherwise undetectable shared owners can limit what the tool can establish. UDP connection facts can be displayed, but local UDP process attribution is not supported in v0.1.

## Install from source

This first release is not published as a crate or prebuilt download. It requires Rust 1.88 or later, then build it:

```sh
git clone https://github.com/HsinyuanZhang/mihomo-top.git
cd mihomo-top
cargo build --release --locked
install -Dm755 target/release/mihomo-top "$HOME/.local/bin/mihomo-top"
```

Ensure `$HOME/.local/bin` is on your `PATH`, then verify the installation:

```sh
mihomo-top --help
```

The binary only connects to a numeric loopback controller: `127.0.0.1:PORT` or `[::1]:PORT`. Remote hosts, host names, TLS, and other controller addresses are intentionally unsupported in v0.1.

## Quick start

Set the controller secret through a protected local file, then start the TUI:

```sh
install -d -m 700 "$HOME/.config/mihomo-top"
install -m 600 /dev/null "$HOME/.config/mihomo-top/secret"
# Write your controller secret to the file using your preferred editor.
mihomo-top --controller 127.0.0.1:9090 --secret-file "$HOME/.config/mihomo-top/secret"
```

`--secret-file` must name a regular, non-symlink file with mode exactly `0600`. Alternatively, set `MIHOMO_SECRET` in the environment. The secret is used only for controller authentication; JSON output reports only a boolean `secret_in_use` and never emits the secret.

Run `mihomo-top` without a subcommand to open the TUI only when both standard input and standard output are terminals. If either is not a terminal, the default invocation streams JSON Lines:

```sh
mihomo-top --controller 127.0.0.1:9090 --secret-file "$HOME/.config/mihomo-top/secret" > frames.jsonl
```

Use `Ctrl-C` or `SIGTERM` to stop. The network worker checks for cancellation on a short interval (currently 250 ms), and tmux lookup has a two-second deadline; this is not a hard end-to-end shutdown guarantee. The monitor reads mihomo, `/proc`, and tmux data without changing them; its expected output is JSON/JSONL on standard output, with diagnostics on standard error. If a signal stops a stream while a pipe is blocked, the final JSONL line can be incomplete. Consumers must ignore an unterminated final line; there is no delivery or durability guarantee.

## Commands and output

| Invocation | Result |
| --- | --- |
| `mihomo-top` | TUI when stdin and stdout are terminals; JSONL stream otherwise |
| `mihomo-top tui` | Force the TUI |
| `mihomo-top --once` | Fetch one frame and print one JSON document |
| `mihomo-top --stream` | Stream one JSON document per line (JSONL) |
| `mihomo-top --format json` | Select JSON for the one-frame mode |
| `mihomo-top --format jsonl` | Select JSONL for streaming mode |
| `mihomo-top --version` / `-V` | Print version |
| `mihomo-top --help` / `-h` | Print help |

`--format json` is valid for one-frame output and `--format jsonl` for stream output. Use the format that matches the selected mode; incompatible mode/format combinations are rejected.

```sh
# One machine-readable snapshot.
mihomo-top --once --controller 127.0.0.1:9090 --secret-file "$HOME/.config/mihomo-top/secret"

# Explicit continuous JSONL stream.
mihomo-top --stream --format jsonl --controller [::1]:9090 --no-tmux

# TUI without any local process or pane lookup.
mihomo-top tui --no-attribution
```

See [the schema reference](docs/output-schema.md) for every field and state.

## Run as a user service

An example systemd user unit is included at [examples/mihomo-top.service](examples/mihomo-top.service). Copy it after installing the binary at `~/.local/bin/mihomo-top`, then reload and start it:

```sh
install -d "$HOME/.config/systemd/user"
cp examples/mihomo-top.service "$HOME/.config/systemd/user/mihomo-top.service"
systemctl --user daemon-reload
systemctl --user enable --now mihomo-top.service
journalctl --user -u mihomo-top.service -f -o cat
```

The supplied unit starts `--stream --no-tmux` and deliberately contains no controller secret. If the controller requires authentication, add `--secret-file PATH` to its `ExecStart` after placing a protected secret file at that path. To enable pane attribution, remove `--no-tmux` and add `--socket PATH` with an appropriate local tmux socket path.

## Options

| Option | Meaning |
| --- | --- |
| `--controller HOST:PORT` | Numeric loopback controller endpoint; accepts `127.0.0.1:PORT` and `[::1]:PORT` only. |
| `--secret-file PATH` | Secret file, regular/non-symlink and mode exactly `0600`. |
| `MIHOMO_SECRET` | Environment fallback when `--secret-file` is absent. |
| `--socket PATH` | tmux socket path; default is the current/default tmux socket. |
| `--no-tmux` | Disable tmux-pane attribution. |
| `--no-attribution` | Disable all local process and pane attribution. |
| `--tick-ms N` | UI/stream refresh interval; default `1000`, valid range `100`–`60000`. |
| `--timeout-secs N` | Time limit for `--once`; default `10`. |

When both a secret file and `MIHOMO_SECRET` are available, the explicit file is used. Do not put a controller secret on the command line, where other local processes may be able to see it.

## Attribution and privacy

Local attribution is optional and performed only on the local Linux host. `--no-attribution` disables it completely; `--no-tmux` keeps process lookup but disables tmux lookup. The monitor does not need root, so sockets owned by other users can remain unattributed. The connection stream and its destination metadata are supplied by the local mihomo controller; the tool does not create connection history or promise complete coverage.

For detail on these boundaries and failure cases, see [docs/limitations.md](docs/limitations.md).

## Testing

Offline checks (`cargo test --locked`) do not need mihomo. The live lab in [TESTING.md](TESTING.md) downloads a pinned mihomo binary, points it at a local mock proxy, and checks per-process status, reported destination, and connectivity:

```sh
scripts/test-env.sh
```

## License

MIT. See [LICENSE](LICENSE).
