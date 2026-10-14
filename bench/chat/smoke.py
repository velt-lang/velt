#!/usr/bin/env python3
"""Smoke test for any chatbench server (contract: SPEC.md).

Usage: python3 smoke.py [--tls] <port> [host]
       python3 smoke.py <base> where <base> is ws://host:port, wss://host:port, http://… or https://…

--tls (or a wss:// / https:// base) speaks TLS, trusting the CA in the PEM file named by env CA
(default: the system's trust store).

Standard library only: raw-socket WebSocket handshake and masked frames. Room names get a random
suffix so the test also works against a server that is not fresh; /stats is checked as deltas.
"""
import base64
import hashlib
import hmac
import json
import os
import socket
import ssl
import struct
import sys
import time
import urllib.parse

SECRET = b"chatbench-secret"


def parse_args(argv):
    tls = "--tls" in argv
    rest = [a for a in argv if a != "--tls"]
    if rest and "://" in rest[0]:
        u = urllib.parse.urlsplit(rest[0])
        if u.scheme not in ("ws", "wss", "http", "https"):
            sys.exit(f"unsupported scheme: {u.scheme}")
        return u.hostname or "127.0.0.1", u.port or (443 if u.scheme in ("wss", "https") else 80), \
            tls or u.scheme in ("wss", "https")
    host = rest[1] if len(rest) > 1 else "127.0.0.1"
    port = int(rest[0]) if rest else 8080
    return host, port, tls


HOST, PORT, TLS = parse_args(sys.argv[1:])
SSL_CTX = ssl.create_default_context(cafile=os.environ.get("CA")) if TLS else None
TAG = os.urandom(3).hex()

failures = 0
checks = 0


def check(cond, what):
    global failures, checks
    checks += 1
    if cond:
        print(f"  ok   {what}")
    else:
        failures += 1
        print(f"  FAIL {what}")
    return cond


def token(user):
    return user + "." + hmac.new(SECRET, user.encode(), hashlib.sha256).hexdigest()


def loads_ordered(s):
    """Parse JSON keeping key order: objects become lists of (key, value) pairs."""
    return json.loads(s, object_pairs_hook=lambda pairs: pairs)


def keys(pairs):
    return [k for k, _ in pairs]


def as_dict(pairs):
    return dict(pairs)


class WS:
    def __init__(self, sock):
        self.sock = sock
        self.buf = b""

    def _recv_exact(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("connection closed")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def send_frame(self, opcode, payload):
        header = bytes([0x80 | opcode])
        n = len(payload)
        if n < 126:
            header += bytes([0x80 | n])
        elif n < 65536:
            header += bytes([0x80 | 126]) + struct.pack("!H", n)
        else:
            header += bytes([0x80 | 127]) + struct.pack("!Q", n)
        mask = os.urandom(4)
        masked = bytes(b ^ mask[i & 3] for i, b in enumerate(payload))
        self.sock.sendall(header + mask + masked)

    def send_text(self, s):
        self.send_frame(0x1, s.encode("utf-8"))

    def send_json(self, obj):
        self.send_text(json.dumps(obj, separators=(",", ":"), ensure_ascii=False))

    def recv(self, timeout=3.0):
        """Next text message as str; None on timeout. Answers pings, skips pongs."""
        self.sock.settimeout(timeout)
        try:
            message = b""
            while True:
                b0, b1 = self._recv_exact(2)
                opcode = b0 & 0x0F
                n = b1 & 0x7F
                if n == 126:
                    n = struct.unpack("!H", self._recv_exact(2))[0]
                elif n == 127:
                    n = struct.unpack("!Q", self._recv_exact(8))[0]
                mask = self._recv_exact(4) if b1 & 0x80 else None
                payload = self._recv_exact(n)
                if mask:
                    payload = bytes(b ^ mask[i & 3] for i, b in enumerate(payload))
                if opcode == 0x8:
                    raise ConnectionError("close frame received")
                if opcode == 0x9:
                    self.send_frame(0xA, payload)
                    continue
                if opcode == 0xA:
                    continue
                message += payload
                if b0 & 0x80:
                    return message.decode("utf-8")
        except socket.timeout:
            return None

    def recv_json(self, timeout=3.0):
        s = self.recv(timeout)
        return None if s is None else loads_ordered(s)

    def close(self):
        try:
            self.send_frame(0x8, struct.pack("!H", 1000))
            self.sock.settimeout(1.0)
            # Wait for the server's close frame (or EOF) so the server has processed it.
            while True:
                b0, b1 = self._recv_exact(2)
                n = b1 & 0x7F
                if n == 126:
                    n = struct.unpack("!H", self._recv_exact(2))[0]
                elif n == 127:
                    n = struct.unpack("!Q", self._recv_exact(8))[0]
                self._recv_exact(n)
                if b0 & 0x0F == 0x8:
                    break
        except (OSError, ConnectionError):
            pass
        finally:
            self.sock.close()


def open_socket():
    """A TCP connection to the server, wrapped in TLS (verified against CA) with --tls."""
    sock = socket.create_connection((HOST, PORT), timeout=5)
    if SSL_CTX is not None:
        sock = SSL_CTX.wrap_socket(sock, server_hostname=HOST)
    return sock


def handshake(query):
    """Returns (status, WS or None)."""
    sock = open_socket()
    key = base64.b64encode(os.urandom(16)).decode()
    req = (
        f"GET /ws?{query} HTTP/1.1\r\n"
        f"Host: {HOST}:{PORT}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "\r\n"
    )
    sock.sendall(req.encode())
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = sock.recv(4096)
        if not chunk:
            break
        data += chunk
    head, _, rest = data.partition(b"\r\n\r\n")
    lines = head.decode("latin-1").split("\r\n")
    try:
        status = int(lines[0].split(" ")[1])
    except (IndexError, ValueError):
        sock.close()
        return 0, None
    if status != 101:
        sock.close()
        return status, None
    headers = {}
    for line in lines[1:]:
        k, _, v = line.partition(":")
        headers[k.strip().lower()] = v.strip()
    accept = base64.b64encode(
        hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()
    ).decode()
    if headers.get("sec-websocket-accept") != accept:
        sock.close()
        return -1, None
    if "permessage-deflate" in headers.get("sec-websocket-extensions", ""):
        print("  note: server negotiated permessage-deflate without being asked")
    ws = WS(sock)
    ws.buf = rest
    return status, ws


def connect(user, room):
    q = urllib.parse.urlencode({"token": token(user), "room": room})
    status, ws = handshake(q)
    if status != 101:
        raise RuntimeError(f"connect {user}@{room}: status {status}")
    welcome = ws.recv_json()
    return ws, welcome


def stats():
    sock = open_socket()
    sock.sendall(f"GET /stats HTTP/1.1\r\nHost: {HOST}:{PORT}\r\nConnection: close\r\n\r\n".encode())
    data = b""
    while True:
        chunk = sock.recv(65536)
        if not chunk:
            break
        data += chunk
    sock.close()
    head, _, body = data.partition(b"\r\n\r\n")
    status = int(head.split(b" ")[1])
    if b"transfer-encoding: chunked" in head.lower():
        out, rest = b"", body
        while True:
            size_line, _, rest = rest.partition(b"\r\n")
            size = int(size_line, 16)
            if size == 0:
                break
            out += rest[:size]
            rest = rest[size + 2 :]
        body = out
    return status, loads_ordered(body.decode())


def wait_stats(pred, timeout=2.0):
    """Polls /stats until pred(dict) holds (closes are processed asynchronously)."""
    deadline = time.time() + timeout
    while True:
        _, s = stats()
        d = as_dict(s)
        if pred(d) or time.time() > deadline:
            return d
        time.sleep(0.05)


def main():
    scheme = "wss" if TLS else "ws"
    print(f"chatbench smoke test against {scheme}://{HOST}:{PORT} (tag {TAG})")

    print("stats")
    status, s0 = stats()
    check(status == 200, "GET /stats -> 200")
    check(keys(s0) == ["connections", "rooms", "messages"], f"/stats keys in order: {keys(s0)}")
    base = as_dict(s0)

    print("auth and validation")
    good = token("alice")
    room_ok = "r" + TAG
    cases = [
        ("missing token", {"room": room_ok}, 401),
        ("bad signature", {"token": "alice." + "0" * 64, "room": room_ok}, 401),
        ("signature for another user", {"token": "bob." + good.split(".")[1], "room": room_ok}, 401),
        ("no dot", {"token": "alice", "room": room_ok}, 401),
        ("uppercase hex signature", {"token": good.upper().replace("ALICE", "alice"), "room": room_ok}, 401),
        ("missing room", {"token": good}, 400),
        ("uppercase room", {"token": good, "room": "Lobby"}, 400),
        ("room with dash", {"token": good, "room": "a-b"}, 400),
        ("room too long (33)", {"token": good, "room": "a" * 33}, 400),
        ("empty room", {"token": good, "room": ""}, 400),
        ("valid token, bad user name", {"token": token("Alice"), "room": room_ok}, 400),
    ]
    for what, params, want in cases:
        status, ws = handshake(urllib.parse.urlencode(params))
        if ws:
            ws.close()
        check(status == want, f"{what} -> {want} (got {status})")

    print("welcome and history")
    hroom = "h" + TAG
    ws, welcome = connect("carol", hroom)
    check(welcome is not None and keys(welcome) == ["type", "user", "room", "history"],
          f"welcome keys in order: {welcome and keys(welcome)}")
    w = as_dict(welcome or [])
    check(w.get("type") == "welcome" and w.get("user") == "carol" and w.get("room") == hroom,
          "welcome type/user/room")
    check(w.get("history") == [], "empty room -> empty history")
    sent_ids = []
    for i in range(25):
        ws.send_json({"type": "say", "text": f"m{i}"})
        m = ws.recv_json()
        if m is not None:
            sent_ids.append(as_dict(m).get("id"))
    check(len(sent_ids) == 25, "sender receives its own 25 messages")
    check(sent_ids == sorted(sent_ids) and len(set(sent_ids)) == 25, "message ids increase")
    ws.close()
    ws, welcome = connect("dave", hroom)
    hist = as_dict(welcome or []).get("history", [])
    check(len(hist) == 20, f"history has 20 entries (got {len(hist)})")
    check(all(keys(e) == ["id", "user", "text", "ts"] for e in hist),
          "history entries have keys id,user,text,ts in order")
    hd = [as_dict(e) for e in hist]
    check([e["text"] for e in hd] == [f"m{i}" for i in range(5, 25)], "history is the last 20, oldest first")
    check([e["id"] for e in hd] == sent_ids[5:], "history ids match the broadcast ids")
    check(all(e["user"] == "carol" for e in hd), "history user")
    now = time.time() * 1000
    check(all(isinstance(e["ts"], int) and abs(e["ts"] - now) < 60_000 for e in hd), "history ts is epoch ms")
    ws.close()

    print("broadcast")
    r1, r2 = "a" + TAG, "b" + TAG
    a, _ = connect("ann", r1)
    b, _ = connect("ben", r1)
    c, _ = connect("cid", r2)
    s = wait_stats(lambda d: d["connections"] == base["connections"] + 3)
    check(s["connections"] == base["connections"] + 3, f"/stats connections +3 (got {s['connections'] - base['connections']})")
    check(s["rooms"] == base["rooms"] + 2, f"/stats rooms +2 (got {s['rooms'] - base['rooms']})")
    check(s["messages"] == base["messages"] + 25, f"/stats messages +25 (got {s['messages'] - base['messages']})")
    a.send_json({"type": "say", "text": "hello, wörld \"quoted\" ✓"})
    ma, mb = a.recv_json(), b.recv_json()
    check(ma is not None and keys(ma) == ["type", "id", "room", "user", "text", "ts"],
          f"message keys in order: {ma and keys(ma)}")
    da, db = as_dict(ma or []), as_dict(mb or [])
    check(da.get("type") == "message" and da.get("room") == r1 and da.get("user") == "ann"
          and da.get("text") == "hello, wörld \"quoted\" ✓", "sender receives the message")
    check(da == db and mb is not None, "other client in the room receives the same message")
    check(isinstance(da.get("id"), int) and isinstance(da.get("ts"), int), "id and ts are integers")
    check(c.recv(timeout=0.5) is None, "client in another room receives nothing")
    b.send_json({"type": "say", "text": "x" * 1000})
    check(as_dict(a.recv_json() or []).get("text") == "x" * 1000, "1000-character text accepted")
    b.recv_json()
    b.send_json({"type": "say", "text": "é" * 1000})
    check(as_dict(a.recv_json() or []).get("text") == "é" * 1000, "1000 two-byte characters accepted")
    b.recv_json()

    print("ping")
    for t in (12345, 1.5, -7, 1728580000123):
        a.send_text(json.dumps({"type": "ping", "t": t}))
        p = a.recv_json()
        check(p is not None and keys(p) == ["type", "t"] and as_dict(p) == {"type": "pong", "t": t},
              f"ping t={t} -> pong t={t} (got {p})")
    check(b.recv(timeout=0.3) is None, "pong goes to the sender only")

    print("errors")
    bad = [
        ("bad JSON", "{not json"),
        ("JSON array", "[1,2]"),
        ("JSON string", '"say"'),
        ("unknown type", '{"type":"shout","text":"hi"}'),
        ("missing type", '{"text":"hi"}'),
        ("say without text", '{"type":"say"}'),
        ("say with number text", '{"type":"say","text":5}'),
        ("say with empty text", '{"type":"say","text":""}'),
        ("say with 1001 characters", json.dumps({"type": "say", "text": "y" * 1001})),
        ("ping without t", '{"type":"ping"}'),
        ("ping with string t", '{"type":"ping","t":"1"}'),
    ]
    for what, payload in bad:
        a.send_text(payload)
        e = a.recv_json()
        check(e is not None and keys(e) == ["type", "error"]
              and as_dict(e) == {"type": "error", "error": "bad message"}, f"{what} -> error")
    check(b.recv(timeout=0.3) is None, "errors are not broadcast")
    a.send_json({"type": "ping", "t": 1})
    check(as_dict(a.recv_json() or []).get("type") == "pong", "connection still usable after errors")

    print("leave")
    s = wait_stats(lambda d: True)
    check(s["messages"] == base["messages"] + 28, f"/stats messages +28 (got {s['messages'] - base['messages']})")
    c.close()
    s = wait_stats(lambda d: d["rooms"] == base["rooms"] + 1)
    check(s["rooms"] == base["rooms"] + 1, "closing the only client of a room removes the room")
    check(s["connections"] == base["connections"] + 2, "connections -1 after close")
    a.close()
    b.close()
    s = wait_stats(lambda d: d["connections"] == base["connections"] and d["rooms"] == base["rooms"])
    check(s["connections"] == base["connections"] and s["rooms"] == base["rooms"],
          "all closed -> connections and rooms back to the start")

    print(f"{checks - failures}/{checks} checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
