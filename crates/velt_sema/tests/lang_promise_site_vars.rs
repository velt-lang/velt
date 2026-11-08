//! Promise types written without an error type get error-set variables (`throws::promise_vars`,
//! #2590): a program that stores rejecting async closures and promises into them is checked in
//! one round, as one storing throwing closures into function types written without `throws` is.
//! Each program is checked on its own, so the process-wide round counter
//! (`velt_sema::check_rounds`) counts its rounds: this file has one test.

mod common;

use common::programs::ok_src;

const HTTP_ERROR: &str = "class HttpError extends Error {\n  status: number;\n  constructor(status: number, message: string) {\n    super(message);\n    this.status = status;\n  }\n}\n";

/// How many rounds checking `src` (after [`HTTP_ERROR`]) takes; it must check.
fn rounds(src: &str) -> usize {
    let before = velt_sema::check_rounds();
    ok_src(&format!("{HTTP_ERROR}{src}"));
    velt_sema::check_rounds() - before
}

/// A handler and a middleware stored through `get` and `use`.
const P0: &str = r#"
type Handler = (path: string) => Promise<string>;
type Next = () => Promise<string>;
type Middleware = (path: string, next: Next) => Promise<string>;

const routes: Handler[] = [];
const middleware: Middleware[] = [];

function get(h: Handler): void { routes.push(h); }
function use(m: Middleware): void { middleware.push(m); }

get(async (path) => {
  if (path == "") { throw new HttpError(400, "empty"); }
  return "ok " + path;
});
use(async (path, next) => {
  const r = await next();
  return "[" + r + "]";
});

async function handle(path: string): Promise<string> {
  const run = async (): Promise<string> => await routes[0](path);
  return await middleware[0](path, run);
}
console.log(await handle("/a"));
"#;

/// Handlers in a `Map`, an array, a field and an object literal.
const P1: &str = r#"
type Handler = (path: string) => Promise<string>;

const byName = new Map<string, Handler>();
const list: Handler[] = [];

class Router {
  fallback: Handler;
  constructor(h: Handler) { this.fallback = h; }
}

interface Route { path: string; run: Handler; }
const routes: Route[] = [];

byName.set("a", async (p) => { if (p == "") { throw new HttpError(400, "map"); } return "map " + p; });
list.push(async (p) => { if (p == "") { throw new HttpError(401, "array"); } return "array " + p; });
const router = new Router(async (p) => { if (p == "") { throw new HttpError(404, "field"); } return "field " + p; });
routes.push({ path: "/x", run: async (p) => { if (p == "") { throw new HttpError(500, "object"); } return "object " + p; } });

async function tryAll(p: string): Promise<void> {
  const hs: Handler[] = [byName.get("a")!, list[0], router.fallback, routes[0].run];
  for (const h of hs) {
    console.log(await h(p));
  }
}
await tryAll("/q");
"#;

/// `new Promise` with `reject`, returned by a function and by a closure stored as a function type.
const P2: &str = r#"
function later(ok: boolean): Promise<string> {
  return new Promise((resolve, reject) => {
    if (ok) { resolve("fine"); } else { reject(new HttpError(503, "later")); }
  });
}
type Make = () => Promise<string>;
const make: Make = () => new Promise((resolve, reject) => reject(new HttpError(502, "direct")));

console.log(await later(true));
console.log(await make());
"#;

/// An async handler made by a closure, awaiting a nested async closure.
const P3: &str = r#"
type Handler = (path: string) => Promise<string>;
type Factory = (name: string) => Handler;

function make(f: Factory): Handler { return f("n"); }

const h = make((name) => async (path) => {
  const inner = async (): Promise<string> => {
    if (path == "") { throw new HttpError(400, name); }
    return name + path;
  };
  return await inner();
});

console.log(await h("/a"));
"#;

/// Generic handler and middleware types.
const P4: &str = r#"
type Handler<T> = (input: T) => Promise<T>;
type Middleware<T> = (input: T, next: Handler<T>) => Promise<T>;

function wrap<T>(m: Middleware<T>, h: Handler<T>): Handler<T> {
  return (x) => m(x, h);
}

const inner: Handler<string> = async (s) => {
  if (s == "") { throw new HttpError(400, "empty"); }
  return s + "!";
};
const outer = wrap<string>(async (s, next) => "<" + (await next(s)) + ">", inner);

console.log(await outer("a"));
"#;

/// Rejections handled by `.then` and `.catch` callbacks.
const P5: &str = r#"
function later(ok: boolean): Promise<string> {
  return new Promise((resolve, reject) => {
    if (ok) { resolve("fine"); } else { reject(new HttpError(503, "later")); }
  });
}
type Handler = (path: string) => Promise<string>;
const h: Handler = async (path) => { if (path == "") { throw new HttpError(400, "empty"); } return path; };
await later(false).then((v) => console.log(v), (e) => console.log(`then ${e}`));
await h("").catch((e) => { console.log(`catch cb ${e}`); return "x"; });
"#;

/// The program of #2590: a `catch` of what the handlers throw still assumes the variables it
/// meets empty (`throws::vars`), so it takes one more round, with the solved sets.
const CAUGHT: &str = r#"
type Handler = (path: string) => Promise<string>;
type Next = () => Promise<string>;
type Middleware = (path: string, next: Next) => Promise<string>;

const routes: Handler[] = [];
const middleware: Middleware[] = [];

function get(h: Handler): void { routes.push(h); }
function use(m: Middleware): void { middleware.push(m); }

get(async (path) => {
  if (path == "") { throw new HttpError(400, "empty"); }
  return "ok " + path;
});
use(async (path, next) => {
  const r = await next();
  return "[" + r + "]";
});

async function handle(path: string): Promise<string> {
  const run = async (): Promise<string> => await routes[0](path);
  return await middleware[0](path, run);
}

try {
  console.log(await handle("/a"));
  console.log(await handle(""));
} catch (e) {
  console.log(`caught ${e}`);
}
"#;

#[test]
fn rejecting_promises_stored_into_promise_types_written_without_errors_take_one_round() {
    for (i, p) in [P0, P1, P2, P3, P4, P5].into_iter().enumerate() {
        assert_eq!(rounds(p), 1, "program P{i}");
    }
    assert_eq!(rounds(CAUGHT), 2);
}
