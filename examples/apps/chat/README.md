# chat

A WebSocket chat: a server that broadcasts to every connected client, and a terminal client.

```sh
velt build
./target/velt/chat serve --port 8080
./target/velt/chat connect alice          # --url ws://127.0.0.1:8080/chat; type lines, /quit
./target/velt/chat connect bob
```

Protocol: one JSON object per text message.

| Direction | Messages |
|---|---|
| client → server | `{"type":"join","name"}` · `{"type":"say","text"}` · `{"type":"leave"}` |
| server → client | `welcome {name, users}` · `joined {name}` · `message {from, text}` · `left {name}` · `error {message}` |

Names are trimmed, 1–20 characters, unique. Malformed messages get an `error` reply; the
connection stays open. A client that disconnects without `leave` is removed like one that left.

| File | What |
|---|---|
| `src/protocol.vlt` | message unions, hand-written decoders (`JSON.parse` can't decode unions), display |
| `src/room.vlt` | `Room`: members, names, sockets (behind a `Mutex` in the server) |
| `src/server.vlt` | `serve` + WebSocket upgrade, one spawned `session` per connection, broadcast |
| `src/client.vlt` | terminal client: a spawned receiver prints, the stdin loop sends |
| `tests/*.test.vlt` | `velt test` (protocol and room, no sockets) |
| `demo.vlt` / `demo.out` | three clients on a real server, step by step; a golden |

Known gap: `WebSocket` is a Copy handle that `close()` frees. The client therefore closes only
from the receiving task, after the server ended the conversation. On the server a broadcast that
has already copied the member list can still start a send to a socket whose session just closed
it (a narrow use-after-free window).
