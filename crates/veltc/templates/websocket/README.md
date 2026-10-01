# {{name}}

A WebSocket chat: a server that broadcasts to every connected client, and a terminal client.

```sh
velt build
./target/velt/{{name}} serve --port 8080
./target/velt/{{name}} connect ann       # --url ws://127.0.0.1:8080/chat; type lines, /quit
./target/velt/{{name}} connect bo
velt test
```

Protocol: one JSON object per text message.

| Direction | Messages |
|---|---|
| client → server | `{"type":"join","name"}` · `{"type":"say","text"}` · `{"type":"leave"}` |
| server → client | `joined {name}` · `message {from, text}` · `left {name}` · `error {message}` |

Names are trimmed, 1–20 characters and unique. A malformed message gets an `error` reply and the
connection stays open.

| File | What |
|---|---|
| `src/protocol.vlt` | message unions, decoders, how the client shows a message |
| `src/room.vlt` | `Room`: members, names, sockets (behind a `Mutex` in the server) |
| `src/server.vlt` | `startChat(port)`: std/http + WebSocket upgrade, one `session` task per client |
| `src/client.vlt` | terminal client: a spawned task prints, the stdin loop sends |
| `src/main.vlt` | `serve` / `connect` subcommands |
| `tests/*.test.vlt` | `velt test`: the protocol, and two clients against a server on port 0 |
