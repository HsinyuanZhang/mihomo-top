# Limitations and scope

`mihomo-top` is a live, best-effort view of the local mihomo controller's current connection data. It is intentionally narrow in v0.1.

## What the data means

- A listed connection is one reported by mihomo at the time of a received frame. The tool does not capture packets itself.
- The cumulative upload/download totals are core totals reported by mihomo. Per-connection values are values in the current controller data, not a durable accounting ledger.
- The tool does not preserve history and makes no completeness guarantee.
- Direct traffic, traffic that bypasses mihomo, and traffic managed by a different proxy/controller do not appear merely because they exist on the machine.
- A frame can become stale while reconnecting, after a parse error, or while the controller is unavailable. Consult `connection_state`, `last_error`, and `stale` in structured output.

## Local attribution

Process and tmux information is optional convenience metadata, not proof of ownership. The program is designed for a same-user Linux session and runs without root.

- TCP attribution may be unavailable when `/proc` data cannot be read, the owning process has exited, the socket belongs to another user, or the socket is shared/ambiguous. In ambiguous cases the result is `UNKNOWN`.
- UDP connection metadata is shown when mihomo reports it, but v0.1 does **not** attribute UDP connections to a local process.
- tmux attribution requires an accessible tmux server/socket and a mapping from an already-attributed local process to a pane. It is disabled by `--no-tmux` and by `--no-attribution`.
- Container, namespace, permission, process-lifetime, and kernel differences can reduce or prevent attribution.

Use `attribution_status` and `attribution_reason` on each row to distinguish a confirmed association from an unavailable or deliberately unsupported one.

## Controller and secret boundary

For v0.1, the controller is numeric loopback only: `127.0.0.1:PORT` or `[::1]:PORT`. DNS names, remote addresses, and TLS are unsupported. This keeps the controller credential and monitoring scope local.

The controller secret comes from `--secret-file` or `MIHOMO_SECRET`. A secret file must be a regular non-symlink with permissions exactly `0600`. Do not pass secrets on the command line or include them in issue reports. Output contains only `secret_in_use`, never the secret value.

## Experimental compatibility

The monitor depends on mihomo's controller connection data and on Linux local process interfaces. Controller payload changes, unavailable controller access, or malformed frames can cause reconnecting, parse-error, stale, or stopped states. Schema version 1 describes `mihomo-top` output; it does not promise compatibility with every mihomo version or fork.
