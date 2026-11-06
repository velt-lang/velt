# {{name}}

A JSON HTTP API: routing, validation, typed errors mapped to status codes, shared state behind a
`Mutex`, and tests that talk to a real server with `fetch`.

```sh
PORT=8080 velt run       # or `velt dev` to restart on every change
curl -X POST localhost:8080/items -d '{"name":"apples","quantity":3}'
curl localhost:8080/items
curl localhost:8080/items/1
curl -X DELETE localhost:8080/items/1
velt test
```

| Route | |
|---|---|
| `GET /health` | `{"ok":true}` |
| `GET /items` | every item |
| `POST /items` `{ name, quantity? }` | create → 201 |
| `GET /items/:id` | one item |
| `DELETE /items/:id` | → 204 |

Errors are `{"error":{"code","message"}}`: 400 (`invalid`, `bad_json`), 404 (`not_found`), 405
(`method_not_allowed`). To add an error, subclass `ApiError` in `src/model.velt`.

| File | What |
|---|---|
| `src/model.velt` | `Item`, request bodies, `ApiError` and its subclasses, validation |
| `src/store.velt` | `ItemStore`: the items in memory |
| `src/api.velt` | `route(store, method, path, body)` → `{ status, body }` |
| `src/server.velt` | `startServer(port)`: std/http + `shared<Mutex<ItemStore>>` |
| `tests/*.test.velt` | `velt test`: validation, and HTTP requests against port 0 |
