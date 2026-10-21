#!/usr/bin/env python3
"""Local lab for mihomo-top: mock IP endpoint, upstream proxy, and clients.

Nothing here dials a real subscription or embeds a credential. The mock
endpoint always answers with the documentation address 203.0.113.10.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import struct
import sys
import threading
import time

FAKE_EGRESS_IP = "203.0.113.10"
HOLD = threading.Event()


def log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def set_comm(name: str) -> None:
    if sys.platform != "linux":
        return
    libc = __import__("ctypes").CDLL("libc.so.6", use_errno=True)
    encoded = name.encode()[:15]
    if libc.prctl(15, encoded, 0, 0, 0) != 0:
        err = __import__("ctypes").get_errno()
        raise OSError(err, f"prctl PR_SET_NAME {name}")


def recvn(sock: socket.socket, count: int) -> bytes:
    buf = b""
    while len(buf) < count:
        chunk = sock.recv(count - len(buf))
        if not chunk:
            raise ConnectionError("short read")
        buf += chunk
    return buf


def read_all(sock: socket.socket, timeout: float) -> bytes:
    sock.settimeout(timeout)
    chunks = []
    while True:
        try:
            chunk = sock.recv(8192)
        except socket.timeout:
            break
        if not chunk:
            break
        chunks.append(chunk)
    return b"".join(chunks)


def split_http(raw: bytes) -> tuple[int, bytes]:
    head, sep, body = raw.partition(b"\r\n\r\n")
    if not sep:
        raise ValueError(f"no HTTP headers in {raw[:120]!r}")
    parts = head.split()
    if len(parts) < 2:
        raise ValueError(f"bad status line {head[:80]!r}")
    return int(parts[1]), body


def emit(payload: dict, result_file: str | None = None) -> None:
    if result_file:
        with open(result_file, "w", encoding="utf-8") as handle:
            json.dump(payload, handle)
            handle.write("\n")
    json.dump(payload, sys.stdout)
    sys.stdout.write("\n")
    sys.stdout.flush()


def proxy_endpoint(value: str) -> tuple[str, int]:
    if "://" in value:
        value = value.split("://", 1)[1]
    host, _, port = value.rpartition(":")
    return host, int(port)


def env_value(*names: str) -> str | None:
    for name in names:
        value = os.environ.get(name)
        if value:
            return value
    return None


def fail_result(scenario: str, proxy_env: str, error: str) -> dict:
    return {
        "scenario": scenario,
        "proxy_env": proxy_env,
        "connectivity": False,
        "egress_ip": None,
        "ok": False,
        "error": error,
        "pid": os.getpid(),
        "comm": comm_now(),
    }


def comm_now() -> str:
    try:
        with open("/proc/self/comm", encoding="utf-8") as handle:
            return handle.read().strip()
    except OSError:
        return ""


def write_ready(path: str | None) -> None:
    if not path:
        return
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(str(os.getpid()))


def direct_http(target_host: str, target_port: int, ready: str | None) -> bytes:
    sock = socket.create_connection((target_host, target_port), timeout=5)
    request = (
        f"GET /hold HTTP/1.1\r\nHost: {target_host}:{target_port}\r\nConnection: close\r\n\r\n"
    )
    sock.sendall(request.encode())
    write_ready(ready)
    return read_all(sock, 30)


def socks_connect(proxy: str, target_host: str, target_port: int) -> socket.socket:
    host, port = proxy_endpoint(proxy)
    sock = socket.create_connection((host, port), timeout=5)
    sock.sendall(b"\x05\x01\x00")
    if recvn(sock, 2) != b"\x05\x00":
        raise ConnectionError("socks authentication rejected")
    try:
        address = socket.inet_aton(target_host)
    except OSError:
        encoded = target_host.encode()
        sock.sendall(b"\x05\x01\x00\x03" + bytes([len(encoded)]) + encoded + struct.pack("!H", target_port))
    else:
        sock.sendall(b"\x05\x01\x00\x01" + address + struct.pack("!H", target_port))
    header = recvn(sock, 4)
    if header[1] != 0:
        raise ConnectionError(f"socks connect failed: {header[1]}")
    atyp = header[3]
    if atyp == 1:
        recvn(sock, 6)
    elif atyp == 4:
        recvn(sock, 18)
    elif atyp == 3:
        length = recvn(sock, 1)[0]
        recvn(sock, length + 2)
    else:
        raise ConnectionError(f"socks address type {atyp}")
    return sock


def body_ip(raw: bytes) -> tuple[bool, str | None, str | None]:
    status, body = split_http(raw)
    text = body.decode("utf-8", "replace").strip()
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return False, None, f"HTTP {status} body {text[:120]!r}"
    ip = parsed.get("ip")
    ok = status == 200 and parsed.get("ok") is True and ip == FAKE_EGRESS_IP
    if ok:
        return True, ip, None
    return False, ip if isinstance(ip, str) else None, f"HTTP {status} body {text[:120]!r}"


def run_client(config: dict) -> dict:
    scenario = config["scenario"]
    comm = config.get("comm") or scenario
    set_comm(comm)
    http_proxy = env_value("HTTP_PROXY", "http_proxy")
    all_proxy = env_value("ALL_PROXY", "all_proxy")
    proxy_env = all_proxy or http_proxy or "none"
    ready = config.get("ready_file")
    target_host = config.get("target_host", "127.0.0.1")
    target_port = int(config.get("target_port", 0))

    if scenario in {"wrong-port", "controller-down"}:
        if not http_proxy:
            return fail_result(scenario, proxy_env, "HTTP_PROXY is not set")
        host, port = proxy_endpoint(http_proxy)
        try:
            socket.create_connection((host, port), timeout=2).close()
        except OSError as exc:
            return {
                "scenario": scenario,
                "proxy_env": proxy_env,
                "connectivity": False,
                "egress_ip": None,
                "ok": False,
                "error": f"{type(exc).__name__}: {exc}",
                "pid": os.getpid(),
                "comm": comm_now(),
            }
        return fail_result(scenario, proxy_env, "closed proxy accepted a connection")

    if scenario == "live-echo":
        if not http_proxy:
            return fail_result(scenario, proxy_env, "HTTP_PROXY is not set")
        host, port = proxy_endpoint(http_proxy)
        sock = socket.create_connection((host, port), timeout=8)
        sock.sendall(
            b"GET http://ifconfig.me/ip HTTP/1.1\r\nHost: ifconfig.me\r\nConnection: close\r\n\r\n"
        )
        status, body = split_http(read_all(sock, 12))
        text = body.decode("utf-8", "replace").strip().split()
        ip = text[0] if text else ""
        try:
            socket.inet_aton(ip)
            parsed = True
        except OSError:
            parsed = False
        return {
            "scenario": scenario,
            "proxy_env": proxy_env,
            "connectivity": status == 200 and parsed,
            "egress_ip": ip if parsed else None,
            "ok": status == 200 and parsed,
            "error": None if status == 200 and parsed else f"HTTP {status} body {body[:80]!r}",
            "pid": os.getpid(),
            "comm": comm_now(),
        }

    if scenario == "live-hold":
        if not all_proxy:
            return fail_result(scenario, proxy_env, "ALL_PROXY is not set")
        sock = socks_connect(all_proxy, target_host, target_port)
        write_ready(ready)
        release = config["release_file"]
        deadline = time.time() + 25
        while time.time() < deadline and not os.path.exists(release):
            time.sleep(0.05)
        sock.close()
        return {
            "scenario": scenario,
            "proxy_env": proxy_env,
            "connectivity": True,
            "egress_ip": None,
            "ok": True,
            "error": None,
            "pid": os.getpid(),
            "comm": comm_now(),
        }

    if scenario == "no-proxy":
        if http_proxy or all_proxy:
            return fail_result(scenario, proxy_env, "no-proxy process inherited a proxy variable")
        raw = direct_http(target_host, target_port, ready)
    elif all_proxy:
        sock = socks_connect(all_proxy, target_host, target_port)
        sock.sendall(
            f"GET /hold HTTP/1.1\r\nHost: {target_host}:{target_port}\r\nConnection: close\r\n\r\n".encode()
        )
        write_ready(ready)
        raw = read_all(sock, 30)
    elif http_proxy:
        # The request is sent inside http_via_proxy; signal ready only after
        # the proxy has the bytes, so the caller waits on the file first.
        host, port = proxy_endpoint(http_proxy)
        sock = socket.create_connection((host, port), timeout=5)
        request = (
            f"GET http://{target_host}:{target_port}/hold HTTP/1.1\r\n"
            f"Host: {target_host}:{target_port}\r\n"
            "Connection: close\r\n"
            "\r\n"
        )
        sock.sendall(request.encode())
        write_ready(ready)
        raw = read_all(sock, 30)
    else:
        return fail_result(scenario, proxy_env, "proxied scenario has no proxy environment")

    ok, ip, error = body_ip(raw)
    return {
        "scenario": scenario,
        "proxy_env": proxy_env,
        "connectivity": ok,
        "egress_ip": ip,
        "ok": ok,
        "error": error,
        "pid": os.getpid(),
        "comm": comm_now(),
    }


def handle_mock(conn: socket.socket) -> None:
    try:
        conn.settimeout(40)
        data = b""
        while b"\r\n\r\n" not in data and len(data) < 65536:
            chunk = conn.recv(4096)
            if not chunk:
                return
            data += chunk
        first = data.split(b"\r\n", 1)[0]
        log(f"mock {first.decode(errors='replace')}")
        if first.startswith(b"GET /release"):
            HOLD.set()
            body = b"ok"
            conn.sendall(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n" + body
            )
            return
        if not HOLD.wait(40):
            log("mock hold timed out")
        body = f'{{"ip":"{FAKE_EGRESS_IP}","ok":true}}\n'.encode()
        header = (
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: "
            + str(len(body)).encode()
            + b"\r\nConnection: close\r\n\r\n"
        )
        conn.sendall(header + body)
    except Exception as exc:  # noqa: BLE001 — keep the lab alive for the next client
        log(f"mock error {type(exc).__name__}: {exc}")
    finally:
        conn.close()


def serve_forever(listener: socket.socket, handler) -> None:
    while True:
        conn, _addr = listener.accept()
        threading.Thread(target=handler, args=(conn,), daemon=True).start()


def handle_upstream(conn: socket.socket) -> None:
    upstream = None
    try:
        conn.settimeout(20)
        data = b""
        while b"\r\n\r\n" not in data and len(data) < 65536:
            chunk = conn.recv(4096)
            if not chunk:
                return
            data += chunk
        head, _, rest = data.partition(b"\r\n\r\n")
        line = head.split(b"\r\n", 1)[0].decode(errors="replace")
        log(f"upstream {line}")
        method, target, *_rest = line.split(" ")
        if method != "CONNECT":
            conn.sendall(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return
        host, _, port_text = target.rpartition(":")
        upstream = socket.create_connection((host, int(port_text)), timeout=5)
        conn.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        if rest:
            upstream.sendall(rest)
        conn.settimeout(None)
        upstream.settimeout(None)

        def pump(src: socket.socket, dst: socket.socket) -> None:
            try:
                while True:
                    buf = src.recv(8192)
                    if not buf:
                        break
                    dst.sendall(buf)
            except Exception:
                pass
            finally:
                try:
                    dst.shutdown(socket.SHUT_WR)
                except OSError:
                    pass

        left = threading.Thread(target=pump, args=(conn, upstream), daemon=True)
        right = threading.Thread(target=pump, args=(upstream, conn), daemon=True)
        left.start()
        right.start()
        left.join()
        right.join()
    except Exception as exc:  # noqa: BLE001
        log(f"upstream error {type(exc).__name__}: {exc}")
    finally:
        if upstream is not None:
            upstream.close()
        conn.close()


def bind_listener() -> socket.socket:
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(64)
    return listener


def cmd_serve(endpoints_file: str) -> None:
    proxy_listener = bind_listener()
    direct_listener = bind_listener()
    upstream_listener = bind_listener()
    payload = {
        "mock_proxy_port": proxy_listener.getsockname()[1],
        "mock_direct_port": direct_listener.getsockname()[1],
        "upstream_port": upstream_listener.getsockname()[1],
    }
    text = json.dumps(payload)
    with open(endpoints_file, "w", encoding="utf-8") as handle:
        handle.write(text + "\n")
    print(text, flush=True)
    threading.Thread(target=serve_forever, args=(proxy_listener, handle_mock), daemon=True).start()
    threading.Thread(target=serve_forever, args=(direct_listener, handle_mock), daemon=True).start()
    threading.Thread(
        target=serve_forever, args=(upstream_listener, handle_upstream), daemon=True
    ).start()
    while True:
        time.sleep(0.5)


def cmd_probe_tun() -> None:
    result = {"tun_open": False, "tun_iface": False, "detail": ""}
    try:
        fd = os.open("/dev/net/tun", os.O_RDWR)
    except OSError as exc:
        result["detail"] = f"open failed: {exc}"
        print(json.dumps(result))
        return
    result["tun_open"] = True
    try:
        import fcntl

        # ifreq: name[16] + flags. IFF_TUN | IFF_NO_PI. Close the fd so the
        # kernel drops the interface; this probe does not route traffic.
        flags = 0x0001 | 0x1000
        ifr = struct.pack("16sH22s", b"mht%d", flags, b"")
        fcntl.ioctl(fd, 0x400454CA, ifr)
        result["tun_iface"] = True
        result["detail"] = "created a transient tun device and closed it"
    except OSError as exc:
        result["detail"] = f"ioctl TUNSETIFF failed: {exc}"
    finally:
        os.close(fd)
    print(json.dumps(result))


def main() -> None:
    parser = argparse.ArgumentParser(description="mihomo-top local lab")
    sub = parser.add_subparsers(dest="cmd", required=True)
    serve = sub.add_parser("serve")
    serve.add_argument("--endpoints-file", required=True)
    client = sub.add_parser("client")
    client.add_argument("--config", required=True)
    sub.add_parser("probe-tun")
    args = parser.parse_args()
    if args.cmd == "serve":
        cmd_serve(args.endpoints_file)
    elif args.cmd == "probe-tun":
        cmd_probe_tun()
    else:
        with open(args.config, encoding="utf-8") as handle:
            config = json.load(handle)
        result_file = config.get("result_file")
        try:
            emit(run_client(config), result_file)
        except Exception as exc:  # noqa: BLE001
            emit(
                fail_result(
                    config.get("scenario", "unknown"),
                    env_value("ALL_PROXY", "HTTP_PROXY") or "none",
                    f"{type(exc).__name__}: {exc}",
                ),
                result_file,
            )
            sys.exit(1)


if __name__ == "__main__":
    main()
