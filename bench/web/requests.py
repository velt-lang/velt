"""Sends COUNT GET requests for ROUTE to 127.0.0.1:PORT one after another on one keep-alive
connection, each after the previous response arrived in full (bench/web/instructions.sh).
Returns once the last response arrived.
Waits up to 60 s for the server to accept connections (it may run under valgrind).

Usage: requests.py PORT ROUTE COUNT
"""

import socket
import sys
import time


def connect(port):
    deadline = time.time() + 60
    while True:
        try:
            return socket.create_connection(("127.0.0.1", port))
        except OSError:
            if time.time() > deadline:
                raise
            time.sleep(0.1)


def read_response(sock, buf):
    """Reads one response (Content-Length or chunked); returns the bytes after it."""
    while b"\r\n\r\n" not in buf:
        buf += recv(sock)
    head, _, rest = buf.partition(b"\r\n\r\n")
    headers = head.decode("latin-1").lower()
    if "transfer-encoding: chunked" in headers:
        while True:
            while b"\r\n" not in rest:
                rest += recv(sock)
            line, _, rest = rest.partition(b"\r\n")
            size = int(line.split(b";")[0], 16)
            while len(rest) < size + 2:
                rest += recv(sock)
            rest = rest[size + 2 :]
            if size == 0:
                return rest
    length = 0
    for line in headers.split("\r\n"):
        if line.startswith("content-length:"):
            length = int(line.split(":", 1)[1])
    while len(rest) < length:
        rest += recv(sock)
    return rest[length:]


def recv(sock):
    data = sock.recv(65536)
    if not data:
        raise ConnectionError("the server closed the connection")
    return data


def main():
    port, route, count = int(sys.argv[1]), sys.argv[2], int(sys.argv[3])
    sock = connect(port)
    request = f"GET {route} HTTP/1.1\r\nHost: localhost\r\n\r\n".encode()
    buf = b""
    for _ in range(count):
        sock.sendall(request)
        buf = read_response(sock, buf)
    sock.close()


if __name__ == "__main__":
    main()
