#!/usr/bin/env python3
"""Smoke test for a chatbench server (see ../SPEC.md). Standard library only.

Usage: smoke_test.py [server-binary]   (default ./chat-go)
Starts the server on a free port with a temporary DB, runs the checks, stops it.
"""
import base64, hashlib, hmac, json, os, socket, struct, subprocess, sys, tempfile, time

HOST = "127.0.0.1"
SECRET = b"chatbench-secret"


def token(user):
    return user + "." + hmac.new(SECRET, user.encode(), hashlib.sha256).hexdigest()


def free_port():
    with socket.socket() as s:
        s.bind((HOST, 0))
        return s.getsockname()[1]


def http_get(port, path, upgrade=False):
    """Returns (status, headers, body-or-leftover, socket)."""
    s = socket.create_connection((HOST, port), timeout=5)
    req = f"GET {path} HTTP/1.1\r\nHost: {HOST}:{port}\r\n"
    if upgrade:
        key = base64.b64encode(os.urandom(16)).decode()
        req += ("Upgrade: websocket\r\nConnection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n")
    else:
        req += "Connection: close\r\n"
    s.sendall((req + "\r\n").encode())
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = s.recv(4096)
        if not chunk:
            break
        buf += chunk
    head, _, rest = buf.partition(b"\r\n\r\n")
    lines = head.decode().split("\r\n")
    status = int(lines[0].split()[1])
    headers = {k.lower(): v.strip() for k, v in (l.split(":", 1) for l in lines[1:])}
    if not upgrade or status != 101:
        n = int(headers.get("content-length", "-1"))
        while n >= 0 and len(rest) < n:
            chunk = s.recv(4096)
            if not chunk:
                break
            rest += chunk
        s.close()
        return status, headers, rest, None
    return status, headers, rest, s


class WS:
    def __init__(self, port, user, room):
        status, _, rest, sock = http_get(port, f"/ws?token={token(user)}&room={room}", upgrade=True)
        assert status == 101, f"expected 101, got {status}"
        self.sock, self.buf = sock, rest

    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def send_text(self, text):
        payload = text.encode()
        mask = os.urandom(4)
        n = len(payload)
        if n < 126:
            hdr = struct.pack("!BB", 0x81, 0x80 | n)
        elif n < 65536:
            hdr = struct.pack("!BBH", 0x81, 0x80 | 126, n)
        else:
            hdr = struct.pack("!BBQ", 0x81, 0x80 | 127, n)
        self.sock.sendall(hdr + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))

    def send(self, obj):
        self.send_text(json.dumps(obj))

    def recv_raw(self, timeout=3.0):
        self.sock.settimeout(timeout)
        b0, b1 = self._read(2)
        assert b0 == 0x81, f"expected unfragmented text frame, got opcode byte {b0:#x}"
        assert not b1 & 0x80, "server frames must not be masked"
        n = b1 & 0x7F
        if n == 126:
            n = struct.unpack("!H", self._read(2))[0]
        elif n == 127:
            n = struct.unpack("!Q", self._read(8))[0]
        return self._read(n).decode()

    def recv(self, timeout=3.0):
        return json.loads(self.recv_raw(timeout))

    def expect_silence(self, timeout=0.3):
        try:
            got = self.recv_raw(timeout)
        except socket.timeout:
            return
        raise AssertionError(f"unexpected frame: {got}")

    def close(self):
        mask = os.urandom(4)
        self.sock.sendall(bytes([0x88, 0x80]) + mask)
        self.sock.close()


def stats(port):
    status, _, body, _ = http_get(port, "/stats")
    assert status == 200
    return body.decode()


def check(name, cond, detail=""):
    if not cond:
        raise AssertionError(f"{name}: {detail}")
    print(f"ok   {name}")


def wait_for(fn, what, timeout=3.0):
    end = time.time() + timeout
    while True:
        v = fn()
        if v or time.time() > end:
            return v
        time.sleep(0.02)


def run(port):
    # --- auth and validation ---
    check("401 without token", http_get(port, "/ws?room=r1", True)[0] == 401)
    check("401 bad signature", http_get(port, "/ws?token=alice.deadbeef&room=r1", True)[0] == 401)
    bad = token("alice")[:-1] + ("0" if token("alice")[-1] != "0" else "1")
    check("401 wrong last hex digit", http_get(port, f"/ws?token={bad}&room=r1", True)[0] == 401)
    check("401 uppercase signature",
          http_get(port, f"/ws?token=alice.{token('alice').split('.')[1].upper()}&room=r1", True)[0] == 401)
    check("400 bad room (uppercase)", http_get(port, f"/ws?token={token('alice')}&room=Room", True)[0] == 400)
    check("400 missing room", http_get(port, f"/ws?token={token('alice')}", True)[0] == 400)
    check("400 room too long", http_get(port, f"/ws?token={token('alice')}&room={'a' * 33}", True)[0] == 400)
    check("400 bad user name", http_get(port, f"/ws?token={token('Bob')}&room=r1", True)[0] == 400)

    check("stats at start", stats(port) == '{"connections":0,"rooms":0,"messages":0}', stats(port))

    # --- empty welcome ---
    a = WS(port, "alice", "r1")
    raw = a.recv_raw()
    check("welcome empty history, exact bytes",
          raw == '{"type":"welcome","user":"alice","room":"r1","history":[]}', raw)

    b = WS(port, "bob", "r1")
    b.recv()
    c = WS(port, "carol", "r2")
    c.recv()
    check("stats with 3 conns / 2 rooms",
          stats(port) == '{"connections":3,"rooms":2,"messages":0}', stats(port))

    # --- say broadcast ---
    a.send({"type": "say", "text": "hello <&> é\U0001F600"})
    ma, mb = a.recv_raw(), b.recv_raw()
    check("sender receives own message", ma == mb, f"{ma} vs {mb}")
    m = json.loads(ma)
    check("message key order", list(m.keys()) == ["type", "id", "room", "user", "text", "ts"], ma)
    check("message fields", m["type"] == "message" and m["room"] == "r1" and m["user"] == "alice"
          and m["text"] == "hello <&> é\U0001F600" and isinstance(m["id"], int)
          and abs(m["ts"] - time.time() * 1000) < 5000, ma)
    check("no HTML escaping, raw UTF-8", "<&>" in ma and "é" in ma, ma)
    c.expect_silence()
    check("other room got nothing", True)

    # --- ping / pong ---
    a.send_text('{"type":"ping","t":1234567.890123}')
    check("pong echoes t exactly", a.recv_raw() == '{"type":"pong","t":1234567.890123}')
    a.send_text('{"type":"ping","t":-5e3}')
    check("pong echoes exponent form", a.recv_raw() == '{"type":"pong","t":-5e3}')
    b.expect_silence()
    check("pong only to sender", True)

    # --- bad messages ---
    err = '{"type":"error","error":"bad message"}'
    for name, frame in [
        ("bad JSON", "{not json"),
        ("unknown type", '{"type":"shout","text":"x"}'),
        ("missing type", '{"text":"x"}'),
        ("empty text", '{"type":"say","text":""}'),
        ("text not a string", '{"type":"say","text":5}'),
        ("missing text", '{"type":"say"}'),
        ("text too long", json.dumps({"type": "say", "text": "x" * 1001})),
        ("ping t not a number", '{"type":"ping","t":"1"}'),
        ("ping without t", '{"type":"ping"}'),
        ("JSON array", "[1,2]"),
    ]:
        a.send_text(frame)
        got = a.recv_raw()
        check(f"error: {name}", got == err, got)
    b.expect_silence()
    a.send({"type": "say", "text": "\U0001F600" * 1000})  # 1000 characters, 4000 bytes
    check("1000-character text accepted", a.recv()["type"] == "message")
    b.recv()

    # --- history: 25 more messages, reconnect, last 20 oldest first ---
    for i in range(25):
        b.send({"type": "say", "text": f"m{i}"})
        a.recv(); b.recv()
    check("stats messages", json.loads(stats(port))["messages"] == 27, stats(port))

    d = WS(port, "dave", "r1")
    w = d.recv()
    texts = [h["text"] for h in w["history"]]
    check("history max 20, oldest first", texts == [f"m{i}" for i in range(5, 25)], texts)
    check("history ids ascending", [h["id"] for h in w["history"]] == sorted(h["id"] for h in w["history"]))
    check("history item key order",
          all(list(h.keys()) == ["id", "user", "text", "ts"] for h in w["history"]), w["history"][0])
    check("history item user", w["history"][0]["user"] == "bob")
    e = WS(port, "erin", "r2")
    w2 = e.recv()
    check("history per room", [h["text"] for h in w2["history"]] == [], w2)

    # --- leave ---
    for x in (a, b, c, d, e):
        x.close()
    s = wait_for(lambda: (lambda st: st if st["connections"] == 0 else None)(json.loads(stats(port))), "drain")
    check("stats after close: 0 conns, 0 rooms, 27 messages",
          s == {"connections": 0, "rooms": 0, "messages": 27}, stats(port))


def main():
    binary = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "./chat-go")
    port = free_port()
    with tempfile.TemporaryDirectory() as tmp:
        env = dict(os.environ, DB=os.path.join(tmp, "chat.db"), HOST=HOST)
        proc = subprocess.Popen([binary, str(port)], env=env, cwd=tmp)
        try:
            if not wait_for(lambda: socket.socket().connect_ex((HOST, port)) == 0, "listen", 10):
                sys.exit("server did not start")
            run(port)
            print("ALL PASSED")
        finally:
            proc.terminate()
            proc.wait()


if __name__ == "__main__":
    main()
