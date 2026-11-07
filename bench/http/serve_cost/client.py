#!/usr/bin/env python3
# client.py <port> <row> <n>: n keep-alive requests of one shape on one connection.
import socket, sys, time

port, row, n = int(sys.argv[1]), sys.argv[2], int(sys.argv[3])
split = 0.005  # after the head, so that the body comes as a packet of its own

def read_response(s, buf):
    while b"\r\n\r\n" not in buf:
        d = s.recv(65536)
        if not d:
            raise SystemExit("closed in a head")
        buf += d
    head, rest = buf.split(b"\r\n\r\n", 1)
    low = head.lower()
    if not low.startswith(b"http/1.1 200"):
        raise SystemExit(head[:80])
    if b"transfer-encoding: chunked" in low:
        while b"0\r\n\r\n" not in rest:
            d = s.recv(65536)
            if not d:
                raise SystemExit("closed in a body")
            rest += d
        return rest.split(b"0\r\n\r\n", 1)[1]
    length = int(low.split(b"content-length: ")[1].split(b"\r\n")[0])
    while len(rest) < length:
        d = s.recv(65536)
        if not d:
            raise SystemExit("closed in a body")
        rest += d
    return rest[length:]

s = socket.create_connection(("127.0.0.1", port))
s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
buf = b""
head = b"POST /text HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\n"
get = ("GET /%s HTTP/1.1\r\nHost: x\r\n\r\n" % row).encode()
for i in range(n):
    if row == "split":
        s.sendall(head)
        time.sleep(split)
        s.sendall(b"hello")
    else:
        s.sendall(get)
    buf = read_response(s, buf)
s.close()
