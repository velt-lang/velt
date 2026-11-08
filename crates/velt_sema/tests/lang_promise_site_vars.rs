//! Promise types written without an error type get error-set variables (`throws::promise_vars`,
//! #2590): a program that stores rejecting async closures and promises into them is checked in
//! one round, as one storing throwing closures into function types written without `throws` is,
//! also when a `catch` catches what they reject with (`throws::var_known`).
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

/// Rejections handled by `.then` and `.catch` callbacks, which read the reason.
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

/// The program of #2590: handlers and middleware stored through `get` and `use`, and a `catch`
/// of what they reject with after them.
const C0: &str = r#"
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

/// Handlers in a `Map`, an array, a field and an object literal, called in a `try` whose
/// `catch` tests the error with `instanceof`.
const C1: &str = r#"
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
    try {
      console.log(await h(p));
    } catch (e) {
      if (e instanceof HttpError) { console.log(`caught ${e.status} ${e.message}`); }
    }
  }
}
await tryAll("/q");
await tryAll("");
"#;

/// An async handler made by a closure, awaiting a nested async closure, in a `try`.
const C2: &str = r#"
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

try { console.log(await h("/a")); console.log(await h("")); } catch (e) { console.log(`caught ${e}`); }
"#;

/// Generic handler and middleware types, in a `try`.
const C3: &str = r#"
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

try { console.log(await outer("a")); console.log(await outer("")); } catch (e) { console.log(`caught ${e}`); }
"#;

/// A handler that rejects with `Late` stored after the `catch` that catches what the handlers
/// reject with was checked, which knew of `HttpError` only: the set that `catch` assumed grows,
/// so the program is checked again with the solved sets, and the `catch` tests for both.
const GROWS: &str = r#"
class Late extends Error {}
type Handler = (path: string) => Promise<string>;
const routes: Handler[] = [];
function get(h: Handler): void { routes.push(h); }

get(async (path) => {
  if (path == "") { throw new HttpError(400, "empty"); }
  return "ok " + path;
});
for (const i of [0, 1]) {
  try {
    console.log(await routes[i](""));
  } catch (e) {
    if (e instanceof Late) {
      console.log("late: " + e.message);
    } else if (e instanceof HttpError) {
      console.log(`http ${e.status}`);
    }
  }
  if (i == 0) {
    get(async (path) => {
      throw new Late("after " + path);
    });
  }
}
"#;

#[test]
fn rejecting_promises_stored_into_promise_types_written_without_errors_take_one_round() {
    // Checked again with the solved sets: the `new Promise` one as often as without variables.
    for (i, (p, n)) in REASONS.into_iter().zip([2, 3, 2, 2]).enumerate() {
        assert_eq!(rounds(p), n, "reasons program {i}");
    }
    for (i, p) in [P0, P1, P2, P3, P4].into_iter().enumerate() {
        assert_eq!(rounds(p), 1, "program P{i}");
    }
    // The callbacks' reasons are values of the variables' type ([`REASONS`]): as often as
    // without variables.
    assert_eq!(rounds(P5), 3);
    for (i, p) in [C0, C1, C2, C3].into_iter().enumerate() {
        assert_eq!(rounds(p), 1, "program C{i}");
    }
    assert_eq!(rounds(GROWS), 2);
}

/// Rejection reasons used as values: a `.catch` callback's parameter, a `.then` rejection
/// callback's, a `Promise.allSettled` result's `reason`, and a reason narrowed by `instanceof`
/// in a `.catch` callback. Their type is the variable's set, which a check with variables
/// doesn't know while it checks them (`throws::var_known`): each program is checked again with
/// the solved sets, where it checks (the golden `promise_written_rejects_reasons` runs them).
const REASONS: [&str; 4] = [
    r#"
type Handler = (path: string) => Promise<string>;
const h: Handler = async (path) => { if (path == "") { throw new HttpError(400, "empty"); } return path; };
console.log(await h("").catch((e) => e.message));
"#,
    r#"
function later(ok: boolean): Promise<string> {
  return new Promise((resolve, reject) => { if (ok) { resolve("fine"); } else { reject(new HttpError(503, "later")); } });
}
console.log(await later(false).then((v) => v, (e) => "rejected: " + e.message));
"#,
    r#"
type Handler = (path: string) => Promise<string>;
const routes: Handler[] = [];
function get(h: Handler): void { routes.push(h); }
get(async (p) => { if (p == "") { throw new HttpError(400, "e"); } return "a" + p; });
const rs = await Promise.allSettled([routes[0]("")]);
for (const r of rs) {
  if (r.status == "rejected") { console.log("reason " + r.reason.message); }
}
"#,
    r#"
type Handler = (path: string) => Promise<string>;
const h: Handler = async (path) => { if (path == "") { throw new HttpError(400, "empty"); } return path; };
console.log(await h("").catch((e) => { if (e instanceof HttpError) { return `s ${e.status}`; } return "x"; }));
"#,
];
