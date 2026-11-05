# web

A small router and middleware on [`http-server`](../http-server/README.md), over the Fetch types:
method routes with path parameters and wildcards, sub-apps, onion middleware, typed per-request
variables, error and not-found handlers, and `cors`, `logger`, `requestId` and `serveStatic`.

This is a reference package, held to full quality: read it as the guide to building a framework on
`http-server`. Its API may change until it is promoted to a supported framework.

```ts
import { serve } from "http-server";
import { App, cors, logger } from "web";

struct Vars {
  user: string;
}

async function main() {
  const app = new App<Vars>(() => ({ user: "anonymous" }));
  app.use(logger());
  app.use(cors());
  app.around(async (c, next) => {
    c.vars.user = c.req.headers.get("x-user") ?? c.vars.user;
    return await next(c);
  });
  app.get("/users/:id", async (c) => c.json({ id: c.param("id"), by: c.vars.user }));
  app.post("/users", async (c) => c.text("made", 201));

  const handler = app.handler();
  await serve({ port: 8080 }, (req: Request) => handler(req));
}
```

## Routes

`app.get`, `post`, `put`, `patch`, `delete` and `on(method, …)` add a route. A pattern is segments
between slashes:

| Segment | Matches | Read with |
|---|---|---|
| `users` | exactly that segment | |
| `:id` | one non-empty segment | `c.param("id")`, percent-decoded |
| a last `*` | the rest of the path, also nothing | `c.param("*")` |

A literal beats a parameter, which beats a wildcard, and a lookup goes back to the next choice when
the first leads nowhere. HEAD takes GET's route when it has none of its own. A path with no route
gets the `notFound` answer (404 by default). A path with routes for other methods gets 405 with
`Allow`. `/a` and `/a/` are different paths.

`app.route("/api", sub)` mounts another `App` with the same variables on a prefix, its middleware
inside this app's.

A handler gets a `Context`: `c.req`, `c.path`, `c.vars`, `c.param(name)`, `c.query(name)`, and
`c.text`, `c.html`, `c.json` and `c.redirect` for a response.

## Middleware

A middleware is a plain function from the handler it wraps to a new handler, over the Fetch types:

```ts
import { Handler, Middleware } from "web";

function poweredBy(name: string): Middleware {
  return (next: Handler): Handler => async (req: Request): Promise<Response> => {
    const res = await next(req);
    res.headers.set("x-powered-by", name);
    return res;
  };
}
```

So it works without this package, on bare `serve`: `serve(opts, poweredBy("velt")(handler))`. There
are two ways to add one:

- **`app.use(mw, prefix?)`** wraps the whole app, for every request under the prefix, routed or
  not, whenever it is added. The first added is the outermost. It runs before routing and has no
  context.
- **`app.around((c, next) => …, prefix?)`** runs after routing and sees the context. It wraps the
  routes added after it and the not-found answer. It calls `await next(c)` for the rest of the onion,
  and can change the response it gets back, or answer without calling it.

The prefix ends on a segment boundary: `/api` covers `/api` and `/api/x`, not `/apix`.

A received request's and a fetched response's headers can't be changed, as in the Fetch standard;
changing them stops the request with `TypeError: immutable`. So a middleware that adds a request
header passes on a new request (`new Request(req, { headers })`, which takes over the body), and a
proxy handler that wants middleware to mark its response wraps it:
`new Response(fetched.body, { status: fetched.status, headers: fetched.headers })`.

### Built in

| Middleware | Does |
|---|---|
| `cors(options?)` | Answers preflights (204) and marks responses for the origins allowed. Never sends credentials with `*` |
| `logger(write?)` | A line per request when its head is ready: `GET /a 200 3ms`. No query, which may hold secrets |
| `requestId(options?)` | An identifier on the request and response headers. A client's is kept only if it is safe to log |
| `serveStatic(root, options?)` | Files under `root`, with `serveFile`'s validators, 304 and `Range`; passes on what it has no file for |

`serveStatic` keeps requests inside `root` (`resolveUnder`) and refuses dotfiles by default. A
directory redirects to its path with a `/`, which answers with `index.html`. A client accepting `br`
or `gzip` gets a `file.br` or `file.gz` next to the file when there is one.

## Errors

What a handler or middleware throws goes to `app.onError((e, c) => …)` (by default logged, then
500), and the middleware outside it still sees that response. A panic isn't an error: `serve` answers
it with a 500 (see `http-server`'s README).

## Building a framework on `http-server`

What this package had to get right, in the order you will meet it:

1. **Handlers copy what they capture, per request.** An async closure gets its own copy of its
   captures on each call, so that requests on different threads can't touch the same objects. A
   route table captured as an ordinary `Map` is copied whole for every request, and 3,000 routes cost
   100 times as much as 3. Put what every request only reads in `shared(...)`: `handler()` builds its
   tree and tables once and reads them in place. A function in `shared(...)` can be called directly
   from every thread, so a table of them needs no lock. A call inside `Mutex.with` would run
   handlers one at a time.
2. **A router holds no functions.** `shared` reads a value in place only when nothing in it can run
   code, so the tree maps a path to a route's number, and the functions are in a table of their own.
3. **Compose once.** `handler()` builds each route's onion when the app starts, so a request makes
   no closure per layer, and a plain middleware's factory runs once.
4. **Pass the context, don't capture it.** A closure that may run on another thread gets a copy of
   a captured context. So `around`'s `next` takes `c`, and the plain middleware, which can't take it,
   runs before the context exists.
5. **State between requests needs an explicit home.** A rate limiter's `Map` in a closure would be
   per-request. The compiler reports most such changes (`http-server`'s README). Use
   `shared(new Mutex(…))`, an `Atomic` or a `ConcurrentMap`.
6. **Scan by index on the request path.** `pathOf` and the router copy only what they keep. Measure
   instructions retired, and compare with the same compiler.
