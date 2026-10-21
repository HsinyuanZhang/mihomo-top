# Structured output schema (version 1)

`mihomo-top --once` emits one JSON object. `mihomo-top --stream` emits one JSON object per line (JSONL). The default invocation selects JSONL when either standard input or standard output is not a terminal. Diagnostics never share standard output with structured data.

Streaming emits a complete snapshot every `--tick-ms`, including while the controller is unavailable. These snapshots act as heartbeats; the stream has no event-delivery, exactly-once, or durability guarantee. A signal can stop a stream while a pipe is blocked, leaving the final JSONL line incomplete. Consumers must ignore an unterminated final line.

Every document has `schema: 1`. Consumers should reject or explicitly handle unknown schema versions, tolerate future additive fields, and use the state fields instead of assuming every frame is current.

## Top-level object

| Field | Type | Meaning |
| --- | --- | --- |
| `schema` | integer | Output schema version; v0.1 emits `1`. |
| `generated` | string | RFC 3339 time at which this document was generated. |
| `scope_note` | string | Human-readable reminder of the monitor's coverage boundary. |
| `controller` | string | Configured numeric loopback controller endpoint. |
| `secret_in_use` | boolean | Whether a controller secret was configured; never the secret itself. |
| `upload_total` | integer | Cumulative upload total reported by mihomo. |
| `download_total` | integer | Cumulative download total reported by mihomo. |
| `connection_count` | integer | Number of connection rows in this document. |
| `connections` | array | Array of row objects. May be empty. |
| `sort_mode` | string | Ordering mode used for `connections`: `bytes_desc` or `newest_first`. |
| `last_frame_at` | string or null | RFC 3339 receipt time of the most recent valid controller frame. |
| `attribution_notes` | array of strings | Global notes about attribution conditions. |
| `stats_snapshot` | object | Summary counters/statistics for the current monitor session. |
| `connection_state` | string | `connecting`, `connected`, `reconnecting`, `parse_error`, or `stopped`. |
| `last_error` | string or null | Most recent non-secret error, if any. |
| `stale` | boolean | True when state is not `connected`, no valid frame has arrived, or the latest frame is older than three seconds. |
| `local_attribution` | boolean | Whether local process/pane attribution was enabled. |

Values are JSON numbers where applicable. Consumers that cannot exactly represent large integer counters should preserve the original JSON number text or use an integer-capable decoder.

### `stats_snapshot`

`stats_snapshot` contains monitor-session counters. Each value is an exact non-negative `u64` integer in the producer; consumers that use floating-point JSON numbers should preserve the original numeric token when exactness matters:

| Counter | Meaning |
| --- | --- |
| `frames` | Valid controller frames accepted by the aggregator. |
| `proc_scans` | Local `/proc` socket/process scans performed for attribution. |
| `pane_refreshes` | tmux pane-map refreshes performed. |
| `ws_reconnects` | WebSocket reconnection attempts. |
| `unresolved_sockets` | Attribution attempts that did not resolve a local socket owner. |
| `dropped_events` | Events dropped when the bounded transport queue is full, plus queued events superseded when the aggregator coalesces them. This includes state events; it does not guarantee delivery of the latest event. |
| `parse_errors` | Controller payloads rejected as invalid connection frames. |

## Connection row

Each object in `connections` contains these fields. Empty or unavailable values are represented by the implementation's documented unavailable value (such as an empty string or `null`); consumers must inspect `attribution_status` and `attribution_reason` before treating local attribution fields as confirmed.

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | Controller connection identifier. |
| `host` | string | Reported destination host; falls back to `destination_ip` when host is absent. |
| `destination_port` | string | Reported destination port. |
| `network` | string | Network type reported by mihomo, such as `tcp` or `udp`. |
| `source_ip` | string | Reported local/source address. |
| `source_port` | string | Reported local/source port. |
| `destination_ip` | string | Reported destination address; may be empty. |
| `process` | string | Locally attributed process description, when available. |
| `session` | string or null | Attributed tmux session, when available. |
| `pane_id` | string or null | Attributed tmux pane identifier, when available. |
| `pid` | integer or null | Attributed process ID, when known. |
| `pid_start_time` | integer or null | Linux `/proc/<pid>/stat` start-time in boot-clock ticks, used with `pid` to avoid PID reuse. |
| `rule` | string | Raw rule name reported by mihomo. User interfaces can combine it with `rule_payload` for display. |
| `rule_payload` | string | Rule payload reported by mihomo. |
| `chain` | string | Display form of the selected outbound chain. |
| `chains` | array of strings | Full outbound chain as reported by mihomo. |
| `upload` | integer | Per-connection upload counter reported by mihomo. |
| `download` | integer | Per-connection download counter reported by mihomo. |
| `age_seconds` | number or null | Age derived from connection start time, when parseable. |
| `attribution_status` | string | Local attribution result, including unavailable/unknown states. |
| `attribution_reason` | string or null | Explanation for the attribution result. |

`attribution_status` is one of `resolved`, `unknown`, `disabled`, `unsupported`, or `ambiguous`. Do not infer a process for UDP in schema version 1: v0.1 explicitly leaves local UDP process attribution unsupported.

## State handling

`connected` means the monitor has a live controller connection. `connecting` and `reconnecting` mean it is attempting to establish or restore one. `parse_error` indicates that a received payload could not be accepted as a valid connection frame. `stopped` indicates shutdown. During non-connected states, prior data can be stale; use `stale`, `last_frame_at`, and `last_error` to decide whether it is usable for the consumer's purpose.

## Example

The following illustrates shape only; it does not identify a live system or destination.

```json
{
  "schema": 1,
  "generated": "2026-01-01T00:00:01Z",
  "scope_note": "Current connections reported by the local mihomo controller.",
  "controller": "127.0.0.1:9090",
  "secret_in_use": true,
  "upload_total": 1200,
  "download_total": 3400,
  "connection_count": 1,
  "connections": [{
    "id": "connection-example",
    "host": "example.invalid",
    "destination_port": "443",
    "network": "tcp",
    "source_ip": "127.0.0.1",
    "source_port": "45000",
    "destination_ip": "203.0.113.10",
    "process": "example-client",
    "session": "work",
    "pane_id": "%1",
    "pid": 1234,
    "pid_start_time": 123456,
    "rule": "MATCH",
    "rule_payload": "",
    "chain": "Proxy",
    "chains": ["Proxy"],
    "upload": 50,
    "download": 100,
    "age_seconds": 1,
    "attribution_status": "resolved",
    "attribution_reason": null
  }],
  "sort_mode": "bytes_desc",
  "last_frame_at": "2026-01-01T00:00:01Z",
  "attribution_notes": [],
  "stats_snapshot": {
    "frames": 1,
    "proc_scans": 1,
    "pane_refreshes": 1,
    "ws_reconnects": 0,
    "unresolved_sockets": 0,
    "dropped_events": 0,
    "parse_errors": 0
  },
  "connection_state": "connected",
  "last_error": null,
  "stale": false,
  "local_attribution": true
}
```
