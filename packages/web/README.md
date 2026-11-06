# web

A small router and middleware on [`http-server`](../http-server/README.md), over the Fetch types:
method routes with path parameters and wildcards, sub-apps, onion middleware, typed per-request
variables, error and not-found handlers, and `cors`, `logger`, `requestId`, `rateLimit`, `bodyLimit`
and `serveStatic`.

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
`Allow`. `/a` and `/a/` are different paths. `on` takes a method in any case.

`app.route("/api", sub)` mounts another `App` with the same variables on a prefix, its middleware
inside this app's. A `/` ending the prefix doesn't count, and the sub-app's `/` is the prefix alone:
`/api`, not `/api/`. `app.route("/", sub)` puts the sub-app's `/y` at `/y`. The sub-app's layer
prefixes join the same way.

### The path

Routes, prefixes, `c.path` and `serveStatic` read one canonical path (`pathOf(req.url)`, exported for
a plain middleware that decides by path), so an escape, a dot segment or a `\` doesn't get past a
check made on another spelling. As a URL parser does, `\` is `/` and `.` and `..` segments are removed, `%2e` included. Each
character has one spelling: an escape of a character that can stand for itself in a path is decoded
(`/%61dmin` is `/admin`), the others stay escaped, in capitals (`%2F` stays inside its segment). The
same goes for the literals and prefixes you write. An empty segment stays, so `//admin` isn't under
`/admin`: it matches no literal or parameter, and `serveStatic` passes it on. Decide on `c.path`, not
on `c.req.url`, which is what the client sent.

**Case isn't folded, so on a case-insensitive disk a prefix doesn't guard files under `serveStatic`.**
On Windows, and on macOS by default, `/PRIVATE/s.txt` isn't under `/private`, but the disk gives
`serveStatic` the file `private/s.txt` for it. Keep files a prefix protects outside `serveStatic`'s
root.

**A parameter is data the client chose.** Once decoded it can hold what the path can't:
`/files/a%2Fb/..%2F..%2Fetc` gives `*` as `a/b/../../etc`. Don't join one to a file path; use
`resolveUnder` or `serveStatic`.

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
| `logger(write?)` | A line per request when its head is ready: `GET /a 200 3ms`, with the canonical path. No query, which may hold secrets |
| `requestId(options?)` | An identifier on the request and response headers. A client's is kept only if it is safe to log |
| `rateLimit({ limit, windowMs, key, maxKeys? })` | Requests per key per fixed window, shared by all requests; 429 with `Retry-After` and `RateLimit-*` headers. Holds at most `maxKeys` keys |
| `bodyLimit(maxBytes)` | 413 for a request body over the limit, without holding more than the limit |
| `serveStatic(root, options?)` | Files under `root`, with `serveFile`'s validators, 304 and `Range`; passes on what it has no file for |

`serveStatic` keeps requests inside `root` (`resolveUnder`) and refuses dotfiles by default. A
directory redirects to its path with a `/`, which answers with `index.html`. A client accepting `br`
or `gzip` gets a `file.br` or `file.gz` next to the file when there is one.

`logger`'s `write` gets its own copy of what it captures on each call, like a handler, and the
compiler doesn't report a change to a copy: `logger((l) => { lines.push(l); })` leaves `lines` empty.
To keep the lines, write to something shared:

```ts
import { App, logger } from "web";

const lines = shared(new Mutex<string[]>([]));
const app = new App<number>(() => 0);
app.use(
  logger((line: string) => {
    lines.with((all) => {
      all.push(line);
    });
  }),
);
```

### `rateLimit`'s key and table

`key` takes only the `Request`, which doesn't hold the client's address. To count by address, set a
header from it in `serve`'s handler, where `info.remoteAddr` is, overwriting whatever the client sent
under that name:

```ts
import { serve, ServeInfo } from "http-server";
import { App, rateLimit } from "web";

const app = new App<number>(() => 0);
app.use(
  rateLimit({
    limit: 100,
    windowMs: 60000,
    key: (req) => req.headers.get("x-client-address") ?? "unknown",
  }),
);
const handler = app.handler();
await serve({ port: 8080 }, (req: Request, info: ServeInfo) => {
  const headers = new Headers(req.headers);
  headers.set("x-client-address", info.remoteAddr.hostname);
  return handler(new Request(req, { headers }));
});
```

Don't key on `x-forwarded-for` or `forwarded` unless a proxy you control sets them, replacing what
the client sent: otherwise a client can send a new value with each request, and each one gets a new
allowance. Behind such a proxy, `info.remoteAddr` is the proxy, and that header is the one to read.

The table holds at most `maxKeys` keys (default 100,000), and a key longer than 256 characters is
kept as its hash, so a client can't grow it past that. When it is full, the windows that ended are
removed, and if there is still no room a new key gets a 429 until a window ends. The keys in the
table go on being counted as before. It fails closed because dropping a live window would hand a
client its allowance back: anyone able to send many keys could reset any count.

The cost is a lockout. One client who can vary the key can fill the table in about a second and hold
new keys out for the rest of the window, and do it again each window. A long window makes that worse,
as the lockout lasts as long as the window. Keys already in the table and routes without the limiter
are not affected. So choose `maxKeys` above the clients you expect in a window, and a key a client
can't vary at will: the address, and for an IPv6 address its /64, not the whole address, as one client
commonly holds a whole /64.

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
   per-request. The compiler reports most such changes (`http-server`'s README), but not one in a
   plain middleware a function returns, or in a callback handed to one (`logger`'s `write`), which
   it doesn't follow to `serve`. A count in a `Map`, an array or a class field kept that way stays at
   1, silently. So does a `ConcurrentMap` captured by value: it compiles, and answers 1, 1, 1. A `let`
   written there (`n++`) currently stops the compiler with an internal error (#2450). Share what
   changes: `shared(new Mutex(…))`, `shared(0 as i64)` or `shared(ConcurrentMap.create(n))`
   (`ConcurrentMap` is in `velt/collections`, with `u64` keys and values by default). A `Mutex` or an
   `Atomic` captured by value is refused at compile time: each call would have its own.
6. **Routes are in `shared(...)`, and so is what they capture.** A regex a route captures is compiled
   on each request: prefer a module `const`, or a check on `c.path` without a regex. The tree holds
   no regex, so a pattern costs a walk of its segments. A `g` or `y` regex a route or an `around`
   middleware captures panics in `handler()`, even one used only for `replace` or `split`: write the
   literal inside the function. A buffer a route captures may be viewed only from a local of the
   function; a view of a copy (`b.slice(0)`) can go anywhere. A handler a plain middleware made
   panics there too (`const h = bodyLimit(5)(ok)`, then `(c) => h(c.req)`), so apply one to part of
   an app with a prefix: `app.use(bodyLimit(5), "/upload")`. Don't build one inside a route to get
   around that: `await rateLimit({ … })(ok)(c.req)` answers, but makes a new table on each request,
   so it never limits. `rateLimit`'s `key` and `now` are callbacks as `logger`'s `write` is: a change
   to what they capture is lost, silently (rule 5).
7. **Scan by index on the request path.** `pathOf` and the router copy only what they keep. Measure
   instructions retired, and compare with the same compiler.
