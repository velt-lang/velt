# todo-api

A JSON REST API over a todo list, grown from `examples/todo_api.vlt`: routing, validation with
typed errors mapped to status codes, shared state behind a `Mutex`, and two interchangeable
stores: a JSON file (atomic rewrite) or a SQLite database.

```sh
PORT=8090 TODO_DB=todos.json velt run
curl -X POST localhost:8090/todos -d '{"title":"write docs","tags":["work"],"priority":"high"}'
curl 'localhost:8090/todos?done=false&tag=work'
curl -X PATCH localhost:8090/todos/1 -d '{"done":true}'
curl -X DELETE localhost:8090/todos/1
```

`TODO_DB` picks the store by its file name: `*.sqlite`, `*.sqlite3` and `*.db` are SQLite
databases, anything else is a JSON file.

```sh
PORT=8090 TODO_DB=todos.sqlite velt run
```

| Route | |
|---|---|
| `GET /todos[?done=true\|false][&tag=t]` | list, filtered |
| `POST /todos` `{ title, priority?, tags? }` | create → 201 |
| `GET /todos/:id` | one todo |
| `PATCH /todos/:id` `{ title?, done?, priority? }` | update (all fields validated before any change) |
| `DELETE /todos/:id` | → 204 |

Errors are `{"error":{"code","message"}}` with 400 (`invalid`, `bad_json`), 404 (`not_found`) or
405 (`method_not_allowed`). Titles are trimmed, 1–200 characters; priority is `low`, `normal`
(default) or `high`; tags are lower-cased and de-duplicated, at most 10. A database failure is a
500 (`db_error`).

**Storage.** The JSON store keeps the list in memory and, after every change, writes the whole list
to `$TODO_DB` (default `todos.json`) via a temp file + rename. The SQLite store (`std/sqlite`)
creates its table on open (`CREATE TABLE IF NOT EXISTS`), runs in WAL mode, prepares each query
once and commits every change as one statement (`INSERT/UPDATE … RETURNING *`); tags are a JSON
array column, filtered with `json_each`. Both hand out ids as max(id) + 1 after a restart. The
router is generic over the `TodoRepository` interface; interface methods can't throw yet, so they
return their `ApiError` and two small adapters wrap the stores' ordinary throwing methods.

| File | What |
|---|---|
| `src/model.vlt` | `Todo`, request bodies, `ApiError` / `ValidationError` / `NotFoundError`, validation |
| `src/store.vlt` | `TodoStore`: list/get/create/update/delete in memory, JSON snapshot |
| `src/sqlite_store.vlt` | `SqliteTodoStore`: the same operations on a SQLite database |
| `src/repository.vlt` | `TodoRepository` interface, `JsonRepository` / `SqliteRepository` adapters |
| `src/api.vlt` | `route(store, method, path, query, body)` → `{ status, body, changed }` (no sockets) |
| `src/server.vlt` | `std/http` server, `shared<Mutex<S>>`, store selection, JSON persistence |
| `tests/*.test.vlt` | `velt test` (router on both stores, validation, SQLite store on a file) |
| `demo.vlt` / `demo.out` | real HTTP session incl. restart; a golden in `cargo test -p veltc --test golden` |
| `demo_sqlite.vlt` / `demo_sqlite.out` | the same session on SQLite, then the rows read back with SQL |
