# pipeline.py <port> <path> <n>: n GET requests of <path> on one connection, pipelined 16 deep
# (as wrk's pipeline.lua), each batch's responses read before the next batch is sent
# (bench/http/count.sh). The responses must be 200s with bodies that end in "Hello, World!".
import socket
import sys

port, path, n = int(sys.argv[1]), sys.argv[2], int(sys.argv[3])
req = ("GET %s HTTP/1.1\r\nHost: localhost\r\n\r\n" % path).encode()
s = socket.create_connection(("127.0.0.1", port))
s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
done = 0
while done < n:
    k = min(16, n - done)
    s.sendall(req * k)
    buf = b""
    while buf.count(b"HTTP/1.1 200") < k:
        data = s.recv(65536)
        if not data:
            sys.exit("connection closed")
        buf += data
    # The last response may still be arriving: wait for its whole body.
    while not buf.endswith(b"Hello, World!"):
        data = s.recv(65536)
        if not data:
            sys.exit("connection closed")
        buf += data
    done += k
