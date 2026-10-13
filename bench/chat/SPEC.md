# chatbench: a realistic WebSocket app, four implementations

The same chat server in Velt, Bun, Node and Go, driven by one Rust load generator (`load/`). It
exercises what real WebSocket apps do besides framing: an auth check per connection, JSON per
message, a database write per message and a read per join, rooms with fan-out, and shared state
across all connections.

## Server contract (every implementation must match it exactly)

- `argv[1]` (or `PORT`): the port; listen on `HOST` (default `127.0.0.1`). `DB` (default
  `chat.db` in the working directory): the SQLite file, deleted and re-created at start.
- SQLite settings, all implementations: `PRAGMA journal_mode=WAL`, `PRAGMA synchronous=NORMAL`,
  table `messages(id INTEGER PRIMARY KEY, room TEXT NOT NULL, user TEXT NOT NULL, text TEXT NOT
  NULL, ts INTEGER NOT NULL)` and index `messages_room_id ON messages(room, id)`. One connection
  (or the platform's default pool); one `INSERT` per message, no batching.
- `GET /stats` → `200`, JSON `{"connections": n, "rooms": n, "messages": n}` (messages =
  messages stored since start).
- `GET /ws?token=T&room=R` with a WebSocket upgrade:
  - **Auth.** `T` is `<user>.<sig>` where `sig` is the lowercase hex HMAC-SHA256 of `<user>` with
    the key `chatbench-secret`. Wrong or missing → `401` (no upgrade). `user` matches
    `[a-z0-9]{1,32}`; `room` matches `[a-z0-9]{1,32}`, else `400`.
  - **Join.** The connection joins room `R` and the server sends
    `{"type":"welcome","user":U,"room":R,"history":[...]}`: the room's last 20 messages, oldest
    first, each `{"id":N,"user":U,"text":S,"ts":N}`, read from SQLite at join.
  - **Client → server** (text frames, JSON):
    - `{"type":"say","text":S}`: `S` is a string of 1–1000 characters (else reply
      `{"type":"error","error":"bad message"}`). The server takes `ts` = milliseconds since the
      epoch, inserts `(room, user, text, ts)`, and sends
      `{"type":"message","id":N,"room":R,"user":U,"text":S,"ts":ts}` (`N` = the row id) to every
      connection in the room, the sender included.
    - `{"type":"ping","t":X}` → `{"type":"pong","t":X}` to the sender only (`X` any JSON number,
      echoed exactly).
    - Anything else (bad JSON, unknown type) → `{"type":"error","error":"bad message"}`.
  - **Leave.** On close or error the connection leaves its room (an empty room is removed).
- JSON is produced by the platform's normal JSON serializer (key order as listed above); text
  is UTF-8; no compression (`permessage-deflate` off).
- Concurrency: whatever is idiomatic for the platform. Velt uses all cores (one process); Go uses
  all cores (goroutines); Bun and Node run one process (their idiomatic single-process server; a
  cluster would need an external broker for rooms, which this benchmark leaves out).

## TLS (`wss://`)

- With env `TLS_CERT` and `TLS_KEY` both set (paths to a PEM certificate chain and a PEM private
  key) a server serves the same routes over HTTPS/WSS on the same port, and nothing over plain
  HTTP; otherwise plain HTTP/WS as above. Everything else in the contract is unchanged.
- Each server uses its platform's normal TLS stack: Velt std/http (rustls; `serve` gets the PEM
  contents, read with `velt:fs`), Go `http.ListenAndServeTLS` (crypto/tls), Node
  `https.createServer` with the same `upgrade` handling, Bun `Bun.serve({ tls })`
  (BoringSSL). Default protocol versions and cipher suites; HTTP/2 may be offered with ALPN,
  but WebSocket clients ask for HTTP/1.1.
- Test material in `tls/` (regenerate with `tls/make.sh`): `ca.pem` (a test CA), `cert.pem` and
  `key.pem` (ECDSA P-256, signed by the CA, SANs `IP:127.0.0.1` and `DNS:localhost`). Clients
  (`chat-load`, `smoke.py`) take a `wss://` base and trust only the CA in the PEM file named by
  env `CA`.

## Load generator scenarios (`load/`)

Clients are `u<i>` with valid tokens, spread round-robin over `rooms` rooms.

| scenario | what |
|---|---|
| `join` | connect `conns` clients as fast as possible (64 handshakes at a time): handshakes/s, time until every welcome arrived |
| `steady` | `conns` clients in `rooms` rooms send `say` messages at `rate` per second in total (spread evenly); every client measures latency from the sender's timestamp (inside `text`) to its own receipt; report deliveries/s, p50/p99/max latency, errors |
| `ramp` | `steady` with the rate doubling every 5 s until p99 > 100 ms or deliveries fall below 95 % of expected: the highest sustained rate |
| `idle` | `conns` connected clients, one ping per client every 30 s: memory and CPU at rest |

The driver records server CPU time, context switches and RSS (`/proc` on Linux, `ps` on macOS).
