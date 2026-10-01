# Building an HTTP server

This guide builds a small JSON API for todos: routing, request bodies, validation, errors as
status codes, shared state, and tests. The server is [`velt:http`](../std/http.md), which runs
on hyper and tokio: HTTP/1.1 with keep-alive, HTTP/2, and HTTPS through rustls. Handlers run
concurrently on every core.

`velt new todo --template api` generates a larger version of the same design, split into
modules, with tests.

## Hello, HTTP

```ts
import { serve, Request, Response } from "velt:http";

async function main() {
  const server = await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    return Response.json({ path: req.path, method: req.method });
  });
  console.log(`listening on http://127.0.0.1:${server.port}`);
}
```

```sh
velt run hello.vlt
curl localhost:8080/hi        # {"path":"/hi","method":"GET"}
```

A listening server keeps the program running after `main` returns, like Node, until
`server.close()`. The handler is an async arrow that receives a `Request` and returns a
`Response`. `Response.json(value)` serializes any value with `JSON.stringify`, which is
generated at compile time for your types.

## A JSON API

```ts
import { serve, Request, Response } from "velt:http";

class ApiError extends Error {
  status: i64;

  constructor(status: i64, message: string) {
    super(message);
    this.status = status;
  }
}

type Todo = { id: i64; title: string; done: bool };

class NewTodo {
  title?: string;                                // clients may leave it out
}

class Store {
  todos: Todo[] = [];
  nextId: i64 = 1;

  add(title: string): Todo {
    const todo: Todo = { id: this.nextId, title, done: false };
    this.nextId++;
    this.todos.push(todo.clone());
    return todo;
  }

  get(id: i64): Todo {
    const todo = this.todos.find((t) => t.id === id);
    if (todo == null) {
      throw new ApiError(404, `no todo ${id}`);
    }
    return todo;
  }
}

type Reply = { status: i64; body: string };

function json<T>(status: i64, value: T): Reply {
  return { status, body: JSON.stringify(value) };
}

function parseBody(body: string): NewTodo {
  try {
    return JSON.parse<NewTodo>(body);
  } catch (e) {                                  // e: JsonError
    throw new ApiError(400, `bad JSON: ${e.message}`);
  }
}

function dispatch(store: Store, method: string, path: string, body: string): Reply {
  const parts = path.split("/").filter((p) => p !== "");
  if (parts.length === 0 || parts[0] !== "todos") {
    throw new ApiError(404, `no route for ${path}`);
  }
  if (parts.length === 2 && method === "GET") {
    return json(200, store.get(parseInt(parts[1], 10) as i64));
  }
  switch (method) {
    case "GET":
      return json(200, store.todos);
    case "POST": {
      const title = parseBody(body).title?.trim() ?? "";
      if (title === "") {
        throw new ApiError(400, "title is required");
      }
      return json(201, store.add(title));
    }
    default:
      throw new ApiError(405, `${method} is not allowed`);
  }
}

function handle(store: Store, method: string, path: string, body: string): Reply {
  try {
    return dispatch(store, method, path, body);
  } catch (e) {                                  // e: ApiError, the only thing dispatch throws
    return json(e.status, { error: e.message });
  }
}

async function main() {
  const store = shared(new Mutex<Store>(new Store()));
  const server = await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    const reply = store.with((s) => handle(s, req.method, req.path, req.body));
    const res = Response.text(reply.body, reply.status);
    res.setHeader("content-type", "application/json");
    return res;
  });
  console.log(`listening on http://127.0.0.1:${server.port}`);
}
```

```sh
$ curl -X POST localhost:8080/todos -d '{"title":" milk "}'
{"id":1,"title":"milk","done":false}
$ curl -X POST localhost:8080/todos -d '{}'
{"error":"title is required"}
$ curl localhost:8080/todos/9
{"error":"no todo 9"}
$ curl -X PUT localhost:8080/todos
{"error":"PUT is not allowed"}
```

What to notice:

- **Typed errors become status codes.** Everything `dispatch` can throw is an `ApiError`, so in
  `handle`'s `catch (e)` the compiler knows `e` is an `ApiError` and `e.status` type-checks.
  `parseBody` turns the `JsonError` that `JSON.parse` throws into an `ApiError`; without that,
  `e` would be `ApiError | JsonError` and reading `e.status` would be a compile error until you
  narrow it.
- **`JSON.parse<NewTodo>`** checks the body's shape: a wrong type is a `JsonError`
  (`expected string at $.title`), and an optional field (`title?: string`) may be missing.
- **Shared state is explicit.** Handlers run concurrently on every core, so they can't modify
  captured variables. The store lives in `shared(new Mutex<Store>(...))`, and
  `store.with((s) => …)` locks it for the duration of the callback. Keep the work inside `with`
  short and synchronous; the callback can't `await`.
- **No global state.** The store is created in `main` and captured by the handler. That is
  also what lets [`velt dev`](hot-reload.md) hot-swap the handler's code while the store keeps
  its data.

## Requests and responses

- `Request`: `method`, `path` (without the query), `query` (without the `?`), `headers`
  (`get(name)` and `has(name)`, case-insensitive) and `body`.
- `Response.text(body, status = 200)`, `Response.json(value, status = 200)`,
  `Response.html(body, status = 200)`, `Response.bytes(data, status = 200)`;
  `res.header(name, value)` adds a header, `res.setHeader(name, value)` replaces one.
- A handler that throws gets a `500 Internal Server Error`, and its error is printed to stderr.
- `server.close()` stops accepting connections and lets in-flight requests finish;
  `await server.shutdown()` also waits for them.

## Testing it

Start the server on port 0 (any free port), read the port from `server.port`, and call it with
`fetch`. A test file in `tests/` (`tests/api.test.vlt`), assuming `main.vlt` was split so that a
`startServer(port)` function in `src/server.vlt` returns the `Server`:

```ts ignore
import { fetch } from "velt:http";
import { startServer } from "../src/server";

export async function test_create_and_validate() {
  const server = await startServer(0);
  const base = `http://127.0.0.1:${server.port}`;
  const created = await fetch(`${base}/todos`, { method: "POST", body: `{"title":"milk"}` });
  assertEq(created.status, 201);
  assertEq(await created.text(), `{"id":1,"title":"milk","done":false}`);
  const invalid = await fetch(`${base}/todos`, { method: "POST", body: "{}" });
  assertEq(invalid.status, 400);
  server.close();
}
```

`velt test` runs it ([Testing](testing.md)).

## HTTPS and HTTP/2

Pass a PEM certificate chain and key, and the server speaks HTTPS and offers HTTP/2 through
ALPN:

```ts ignore
const server = await serve({ port: 8443, tls: { cert: certPem, key: keyPem } }, handler);
```

`fetch` speaks `http://` and `https://` (trusting Mozilla's root certificates plus the PEM CAs
you pass in `ca`), and uses HTTP/2 when the server offers it. WebSockets are in
[`velt:websocket`](../std/websocket.md).

## Databases

[`velt:postgres`](../std/postgres.md) (async, with a connection pool), [`velt:sqlite`](../std/sqlite.md)
(embedded) and [`velt:redis`](../std/redis.md) decode rows straight into your classes with the
same compile-time JSON machinery: `const users: User[] = await db.query("SELECT …", [id])`.

## Performance

The TechEmpower-style suite in `bench/web` runs all six test types against Rust (axum),
Go (`net/http`), Bun and Node. On Linux arm64 (Debian 12 containers on an Apple M4, 10 cores
shared by client, servers and PostgreSQL; [bench/web/RESULTS.md](../../bench/web/RESULTS.md)):

| Test | Velt | Rust (axum) | Go | Bun | Node |
|---|---:|---:|---:|---:|---:|
| JSON | 741k req/s, 49 MB | 619k req/s, 29 MB | 327k req/s | 248k req/s | 100k req/s, 173 MB |
| single query | 141k req/s, 25 MB | 133k req/s, 34 MB | 119k req/s | 42k req/s | 39k req/s, 188 MB |
| fortunes | 115k req/s, 28 MB | 118k req/s, 40 MB | 89k req/s | 33k req/s | 23k req/s, 278 MB |

Across every database test Velt is within 0.94–1.07× of Rust. Go leads on multi-query tests
with 5 or more queries per request, because its driver batches them into one round trip.
