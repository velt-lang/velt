//! Catching a Velt panic that unwinds (docs/internals/design/unwinding.md), built with each
//! backend: `catchUnwind` returns the panic's message and the process goes on, a panic outside
//! any catch still ends the process, and another language's exception passes the catch (macOS
//! and Linux: on Windows `catchUnwind` is a plain call until its structured exceptions are built). The
//! probe module re-exports std's internal `catchVeltPanic`, so std is copied with it added (as
//! gated_dropped.rs does).

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod test_dir;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

/// The test's std: a copy holding `PROBE` as `velt/zz_unwind`.
fn std_dir(dir: &test_dir::TestDir) -> PathBuf {
    let std: PathBuf = dir.path().join("std");
    if !std.exists() {
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
            &std,
        );
        std::fs::create_dir_all(std.join("zz_unwind")).unwrap();
        std::fs::write(std.join("zz_unwind/index.velt"), PROBE).unwrap();
    }
    std
}

/// `velt <args> main.velt`, with a std holding `PROBE` as `velt/zz_unwind`.
fn velt(dir: &test_dir::TestDir, main: &str, args: &[&str]) -> Output {
    let std = std_dir(dir);
    let file = dir.path().join("main.velt");
    std::fs::write(&file, main).unwrap();
    let mut all: Vec<&str> = args.to_vec();
    all.push(file.to_str().unwrap());
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(&all)
        .env("VELT_STD", &std)
        .current_dir(dir.path())
        .output()
        .unwrap()
}

/// std's catch, and an exception of another language (class `OTHRLANG`) raised with the
/// platform's unwinder. Its payload after the 32-byte header reads as a message, so a catch that
/// took it by mistake hands back `FOREIGN-CAUGHT`.
const PROBE: &str = r#"
import { Ptr, StrBytes } from "velt/ffi";
import { ptrAddress, ptrOf } from "velt/ffi/ptr";

export { catchVeltPanic } from "velt/task/unwind";

declare c("c") function malloc(n: usize): Ptr<u8> | null;
declare c("c") function memset(p: Ptr<u8>, c: i32, n: usize): Ptr<u8>;
declare c("c") function memcpy(dst: Ptr<u8>, src: StrBytes, n: usize): Ptr<u8>;
declare c("c") function _Unwind_RaiseException(e: Ptr<u8>): i32;
declare function velt_rt_flush(): void;

export function raiseForeign(): void {
  velt_rt_flush();
  // SAFETY: a zeroed header of 32 bytes with a class, which the unwinder only reads, then a
  // NUL-terminated text inside the allocation.
  unsafe {
    const e = malloc(48);
    if (e === null) {
      return;
    }
    memset(e, 0, 48);
    memcpy(e, "OTHRLANG", 8);
    memcpy(ptrOf<u8>(ptrAddress(e) + 32), "FOREIGN-CAUGHT", 14);
    _Unwind_RaiseException(e);
  }
}
"#;

#[cfg(not(windows))]
const MAIN: &str = r#"
import { catchVeltPanic } from "velt/zz_unwind";

function deep(n: number, xs: number[]): number {
  if (n == 0) {
    return xs[xs.length + 3];
  }
  return deep(n - 1, xs) + 1;
}

function first(m: string | null): string {
  if (m === null) {
    return "null";
  }
  return m.split(" at ")[0];
}

function main() {
  let ran = 0;
  console.log(first(catchVeltPanic(() => { ran = ran + 1; })), ran);
  console.log(first(catchVeltPanic(() => { panic("boom"); })));
  let after = 0;
  console.log(first(catchVeltPanic(() => { deep(30, [1, 2, 3]); after = 1; })), after);
  let inner: string | null = null;
  const outer = catchVeltPanic(() => { inner = catchVeltPanic(() => { panic("inner"); }); });
  console.log(first(outer), first(inner));
  panic("uncaught");
}
"#;

#[cfg(not(windows))]
const OUT: &str = "null 1
boom
index out of bounds: the len is 3 but the index is 6 0
null inner
";

/// Another language's exception raised inside a catch.
#[cfg(not(windows))]
const FOREIGN: &str = r#"
import { catchVeltPanic, raiseForeign } from "velt/zz_unwind";

function first(m: string | null): string {
  return m === null ? "null" : m;
}

function main() {
  console.log("raising");
  let returned = false;
  const foreign = catchVeltPanic(() => { raiseForeign(); returned = true; });
  console.log(first(foreign), returned);
}
"#;

#[cfg(not(windows))]
#[test]
fn a_velt_panic_is_caught_with_every_backend() {
    let dir = test_dir::TestDir::new();
    for args in [&["run"][..], &["run", "--release"][..]] {
        let out = velt(&dir, MAIN, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            OUT,
            "{args:?}: {stderr}"
        );
        // The uncaught panic ends the process as before; each caught one was printed too.
        assert_eq!(out.status.code(), Some(101), "{args:?}: {stderr}");
        for msg in ["panic: boom", "panic: inner", "panic: uncaught"] {
            assert!(stderr.contains(msg), "{args:?}: {stderr}");
        }
    }
}

/// An exception of another class passes `catchVeltPanic`: the catch never returns it. What
/// happens next depends on the frames above `main`, which this test doesn't rely on: with no
/// frame claiming it the raise returns and the program goes on (`null true`); a Rust frame above
/// `main` (an `extern "C"` function's abort-on-unwind pad, which Rust's personality claims for
/// any class) ends the process there. Either way the catch didn't take it: a catch that claims
/// every class hands back the payload as the message, `FOREIGN-CAUGHT`.
#[cfg(not(windows))]
#[test]
fn another_languages_exception_passes_the_catch() {
    let dir = test_dir::TestDir::new();
    for args in [&["run"][..], &["run", "--release"][..]] {
        let out = velt(&dir, FOREIGN, args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = format!("{args:?}: {:?}\n{stdout}{stderr}", out.status);
        assert!(!stdout.contains("FOREIGN-CAUGHT"), "{why}");
        match stdout.as_ref() {
            "raising\nnull true\n" => assert!(out.status.success(), "{why}"),
            "raising\n" => assert!(!out.status.success(), "{why}"),
            _ => panic!("{why}"),
        }
    }
}

/// A program that never calls `catchUnwind` keeps calling the runtime's panic directly: the
/// std panic path isn't even compiled in.
#[test]
fn a_program_without_catch_keeps_the_runtime_panic() {
    let dir = test_dir::TestDir::new();
    let out = velt(
        &dir,
        "const xs = [1];\nconsole.log(xs[3]);\n",
        &["build", "--emit", "vir"],
    );
    let vir = String::from_utf8_lossy(&out.stdout);
    assert!(vir.contains("velt_rt_panic"), "{vir}");
    assert!(
        !vir.contains("panicRaise") && !vir.contains("velt_eh_try"),
        "{vir}"
    );
}

/// Frames that own values, 20 deep under the catch: strings, arrays, class instances (one with a
/// `[Symbol.dispose]()` that prints) and a closure's environment. Cleanup pads drop them all as
/// the panic unwinds (#2667), so the leak counters balance.
#[cfg(not(windows))]
const OWNING: &str = r#"
import { catchVeltPanic } from "velt/zz_unwind";

class Item {
  name: string;
  items: number[];
  constructor(name: string) {
    this.name = name;
    this.items = [1, 2, 3];
  }
}

class Guard {
  n: number;
  constructor(n: number) {
    this.n = n;
  }
  [Symbol.dispose]() {
    if (this.n % 10 == 0) {
      console.log(`disposed ${this.n}`);
    }
  }
}

function deep(n: number, xs: number[]): number {
  const s = "frame " + n.toString();
  const a = [n, n + 1, n + 2];
  const item = new Item(s + "!");
  const g = new Guard(n);
  const f = () => s.length + a.length;
  if (n == 0) {
    return xs[xs.length + 3];
  }
  return deep(n - 1, xs) + f() + item.items.length + g.n;
}

function main() {
  for (let i = 0; i < 3; i++) {
    const m = catchVeltPanic(() => { deep(20, [1, 2, 3]); });
    console.log(m === null ? "null" : m.split(" at ")[0]);
  }
}
"#;

#[cfg(not(windows))]
const OWNING_OUT: &str = "disposed 0
disposed 10
disposed 20
index out of bounds: the len is 3 but the index is 6
";

/// The debug runtime next to the `velt` binary: it counts blocks (`VELT_RC_STATS`), also for a
/// release build of the program.
#[cfg(not(windows))]
fn debug_runtime() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_velt"))
        .parent()
        .unwrap()
        .join("libvelt_rt.a")
}

/// `(allocated, freed)` of the `blocks=` and the string `alloc=`/`free=` counts.
#[cfg(not(windows))]
fn counts(stderr: &str) -> Option<((u64, u64), (u64, u64))> {
    let line = stderr.lines().rfind(|l| l.starts_with("rc stats:"))?;
    let field = |key: &str| {
        let at = line.find(key)? + key.len();
        let v = line[at..].split(' ').next()?;
        Some(v.to_string())
    };
    let num = |key: &str| field(key)?.parse::<u64>().ok();
    let blocks = field(" blocks=")?;
    let (a, f) = blocks.split_once('/')?;
    Some((
        (a.parse().ok()?, f.parse().ok()?),
        (num(" alloc=")?, num(" free=")?),
    ))
}

/// `velt <args> main.velt` (with `PROBE`) linked with the debug runtime, which counts blocks and
/// strings and reports a use after free or a double free (`velt debug-alloc:`).
#[cfg(not(windows))]
fn velt_counted(dir: &test_dir::TestDir, main: &str, args: &[&str]) -> Output {
    let std = std_dir(dir);
    let file = dir.path().join("main.velt");
    std::fs::write(&file, main).unwrap();
    let mut all: Vec<&str> = args.to_vec();
    all.push(file.to_str().unwrap());
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(&all)
        .env("VELT_STD", &std)
        .env("VELT_RC_STATS", "1")
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env("VELT_RT_LIB", debug_runtime())
        .current_dir(dir.path())
        .output()
        .unwrap()
}

/// `main` prints `want` and ends with every block and string freed, with each of `runs`.
#[cfg(not(windows))]
fn assert_balanced(main: &str, runs: &[&[&str]], want: &str) {
    let dir = test_dir::TestDir::new();
    for args in runs {
        let out = velt_counted(&dir, main, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = format!("{args:?}: {:?}: {stderr}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{why}");
        assert!(out.status.success(), "{why}");
        assert!(!stderr.contains("debug-alloc"), "{why}");
        let ((allocated, freed), (strings, strings_freed)) =
            counts(&stderr).unwrap_or_else(|| panic!("{why}: no counts"));
        assert_eq!(allocated, freed, "{why}: blocks leaked");
        assert_eq!(strings, strings_freed, "{why}: strings leaked");
    }
}

#[cfg(not(windows))]
const DEBUG_AND_RELEASE: &[&[&str]] = &[&["run"], &["run", "--release"]];

#[cfg(not(windows))]
#[test]
fn unwound_frames_drop_what_they_own() {
    let mut want = String::new();
    for _ in 0..3 {
        want.push_str(OWNING_OUT);
    }
    assert_balanced(OWNING, DEBUG_AND_RELEASE, &want);
}

/// A `noalias` param's fields kept in locals (velt_opt noalias.rs) are stored back on the unwind
/// edge: `drain` pops strings out of an array its caller owns, then `boom` panics, and the
/// closure's pad drops the array by its current length, not by the one `drain` loaded (a use
/// after free). Release only: debug builds run no velt_opt.
#[cfg(not(windows))]
const PROMOTED: &str = r#"
import { catchVeltPanic } from "velt/zz_unwind";

function boom(i: number): number {
  if (i > 1000) {
    return boom(i - 1);
  }
  if (i == 7) {
    const xs = [1];
    return xs[i];
  }
  return i;
}

function drain(xs: string[], n: number): number {
  if (n > 1000) {
    return drain(xs, n - 1);
  }
  let t = 0;
  for (let i = 0; i < n; i++) {
    const p = xs.pop();
    if (p !== undefined) {
      t += p.length;
    }
    t += boom(i);
  }
  return t;
}

function main() {
  for (let r = 0; r < 3; r++) {
    const m = catchVeltPanic(() => {
      const xs: string[] = [];
      for (let i = 0; i < 20; i++) {
        xs.push("s" + i.toString() + "-padding-padding");
      }
      console.log(drain(xs, 20) + xs.length);
    });
    console.log(m === null ? "null" : "caught");
  }
}
"#;

#[cfg(not(windows))]
#[test]
fn promoted_fields_are_current_when_a_call_unwinds() {
    assert_balanced(
        PROMOTED,
        &[&["run", "--release"]],
        "caught\ncaught\ncaught\n",
    );
}

/// A panic inside a generator's body, reached through `for...of` over a direct call (the state in
/// the caller's frame), a stored generator's `next()` and `next(value)`. The body's frame marks
/// the generator finished as the panic leaves it, so the pad of the frame holding the generator
/// drops it quietly instead of closing a running generator (which panics, and aborts during a
/// cleanup), and `next()` after the catch reports done. What the body's state held leaks with it,
/// as a panicked async state machine's does.
#[cfg(not(windows))]
const GENERATORS: &str = r#"
import { catchVeltPanic } from "velt/zz_unwind";

function boom(i: number): number { if (i > 5) { const xs = [1]; return xs[i]; } return i; }

function* gen(n: number): Generator<number> {
  const s = "gen" + n.toString();
  for (let i = 0; i < 10; i++) {
    yield s.length + boom(i);
  }
}

function useGen(n: number): number {
  let t = 0;
  for (const v of gen(n)) { t += v; }
  return t;
}

function* sum(): Generator<number, void, number> {
  let total = 0;
  const s = "sum";
  while (true) {
    const x = yield total;
    total += boom(x) + s.length;
  }
}

function main() {
  console.log("embedded", catchVeltPanic(() => { console.log(useGen(1)); }) !== null);
  const g = gen(2);
  const boxed = catchVeltPanic(() => {
    let r = g.next();
    while (!r.done) { r = g.next(); }
  });
  console.log("boxed", boxed !== null, g.next().done);
  const h = sum();
  h.next(0);
  const sent = catchVeltPanic(() => { h.next(1); h.next(9); });
  console.log("send", sent !== null, h.next(1).done);
  console.log("after");
}
"#;

#[cfg(not(windows))]
#[test]
fn a_panic_in_a_generator_body_is_caught() {
    let dir = test_dir::TestDir::new();
    for args in DEBUG_AND_RELEASE {
        let out = velt_counted(&dir, GENERATORS, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = format!("{args:?}: {:?}: {stderr}", out.status);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "embedded true\nboxed true true\nsend true true\nafter\n",
            "{why}"
        );
        assert!(out.status.success(), "{why}");
        assert!(!stderr.contains("debug-alloc"), "{why}");
    }
}

/// Drops on exit edges whose disposer panics: the pad of a drop on an edge drops only what that
/// edge hasn't dropped yet. A `return` dropping two locals (the second's dispose panics), a
/// block ending normally the same way (the first is still dropped), a reassignment whose old
/// value's dispose panics, a `finally` that panics after the `return` in its `try` dropped the
/// `try`'s locals, and one on a `break`.
#[cfg(not(windows))]
const EXIT_EDGES: &str = r#"
import { catchVeltPanic } from "velt/zz_unwind";

function boom(i: number): number { if (i > 5) { const xs = [1]; return xs[i]; } return i; }

class Noisy {
  n: number;
  s: string;
  constructor(n: number) { this.n = n; this.s = "noisy" + n.toString(); }
  [Symbol.dispose]() {
    console.log("dispose " + this.n.toString());
    if (this.n == 2) { boom(9); }
  }
}

function early(c: boolean): number {
  const a = new Noisy(1);
  const b = new Noisy(2);
  if (c) {
    return 5;
  }
  return 6;
}

function block(n: number): number {
  const s = "block" + n.toString();
  {
    const a = new Noisy(1);
    const b = new Noisy(2);
  }
  return s.length;
}

function reassign(n: number): number {
  let x = new Noisy(2);
  x = new Noisy(n);
  return x.n;
}

function fin(n: number): number {
  const s = "fin" + n.toString();
  try {
    const a = new Noisy(3);
    const t = "inner" + n.toString();
    return a.n + t.length;
  } finally {
    console.log("finally " + s);
    boom(n);
  }
}

function brk(n: number): number {
  let t = 0;
  for (let i = 0; i < 3; i++) {
    const b = new Noisy(4);
    try {
      if (i == 1) { break; }
      t += b.n;
    } finally {
      t += boom(n + i);
    }
  }
  return t;
}

function main() {
  console.log("A", catchVeltPanic(() => { console.log(early(true)); }) !== null);
  console.log("B", catchVeltPanic(() => { console.log(block(1)); }) !== null);
  console.log("C", catchVeltPanic(() => { console.log(reassign(5)); }) !== null);
  console.log("D", catchVeltPanic(() => { console.log(fin(9)); }) !== null);
  console.log("E", catchVeltPanic(() => { console.log(brk(5)); }) !== null);
}
"#;

#[cfg(not(windows))]
const EXIT_EDGES_OUT: &str = "dispose 2
dispose 1
A true
dispose 2
dispose 1
B true
dispose 2
dispose 5
C true
dispose 3
finally fin9
D true
dispose 4
dispose 4
E true
";

#[cfg(not(windows))]
#[test]
fn pads_on_exit_edges_drop_only_what_the_edge_has_not() {
    assert_balanced(EXIT_EDGES, DEBUG_AND_RELEASE, EXIT_EDGES_OUT);
}

/// A value whose transfer panics (a function value that captured a weak collection, going to
/// `spawn`): the frame owns it while the transfer glue runs, so the pad drops it, as far as it
/// was transferred (`keep`'s copy, the collection, the environment).
#[cfg(not(windows))]
const TRANSFER: &str = r#"
import { catchVeltPanic } from "velt/zz_unwind";

class V {
  n: number;
  constructor(n: number) {
    this.n = n;
  }
}

class Cache extends WeakMap<V, V> {}

function mk(keep: V): () => number {
  const c = new Cache();
  c.set(keep, keep);
  return () => {
    const x = c.get(keep);
    return x === undefined ? -1 : x.n;
  };
}

async function run(f: () => number): Promise<number> {
  return f();
}

async function main() {
  const keep = new V(7);
  for (let i = 0; i < 2; i++) {
    const msg = catchVeltPanic(() => {
      spawn(run(mk(keep)));
    });
    console.log(msg !== null && msg.includes("WeakMap"), keep.n);
  }
}
"#;

#[cfg(not(windows))]
#[test]
fn a_value_whose_transfer_panics_is_dropped() {
    assert_balanced(TRANSFER, DEBUG_AND_RELEASE, "true 7\ntrue 7\n");
}

/// A resource whose own `clone()` makes a copy holding a function value that captured a weak
/// collection: settling the copy (transferring it in turn) panics after the copy was made. The
/// original's release is deferred only once the copy is settled, so the panic leaves it owned
/// once, by `W`'s temporary, and the pad drops it once (no double release with the transfer's
/// end in `caught()`); the half-settled copy is owned by the glue's frame, so its pad drops it.
#[cfg(not(windows))]
const SETTLE: &str = r#"
import { catchVeltPanic } from "velt/zz_unwind";

class V {
  n: number;
  constructor(n: number) {
    this.n = n;
  }
}

class Cache extends WeakMap<V, V> {}

class R {
  n: number;
  f: (() => number) | null = null;
  constructor(n: number) {
    this.n = n;
  }
  clone(): R {
    const r = new R(this.n);
    const c = new Cache();
    const k = new V(this.n);
    c.set(k, k);
    r.f = () => {
      const x = c.get(k);
      return x === undefined ? -1 : x.n;
    };
    return r;
  }
  [Symbol.dispose]() {}
}

class W {
  r: R;
  constructor(r: R) {
    this.r = r;
  }
}

async function run(w: W): Promise<number> {
  return w.r.n;
}

async function main() {
  const r = new R(5);
  for (let i = 0; i < 2; i++) {
    const msg = catchVeltPanic(() => {
      spawn(run(new W(r)));
    });
    console.log(msg !== null && msg.includes("WeakMap"), r.n);
  }
}
"#;

/// A resource whose own `clone()` returns an object it also stored (`this.saved`): the check
/// after the copy panics, with the original still in its holder; it must not also be in the
/// transfer's deferred list. The copy's reference is released before the panic.
#[cfg(not(windows))]
const SHARED_CLONE: &str = r#"
import { catchVeltPanic } from "velt/zz_unwind";

class R {
  n: number;
  saved: R | null = null;
  constructor(n: number) {
    this.n = n;
  }
  clone(): R {
    const r = new R(this.n);
    this.saved = r;
    return r;
  }
  [Symbol.dispose]() {}
}

class W {
  r: R;
  constructor(r: R) {
    this.r = r;
  }
}

async function run(w: W): Promise<number> {
  return w.r.n;
}

async function main() {
  const r = new R(3);
  for (let i = 0; i < 2; i++) {
    const msg = catchVeltPanic(() => {
      spawn(run(new W(r)));
    });
    console.log(msg !== null && msg.includes("still referenced elsewhere"), r.n);
  }
}
"#;

#[cfg(not(windows))]
#[test]
fn a_copy_whose_settling_panics_releases_the_original_once() {
    assert_balanced(SETTLE, DEBUG_AND_RELEASE, "true 5\ntrue 5\n");
}

#[cfg(not(windows))]
#[test]
fn a_clone_returning_a_stored_object_releases_the_original_once() {
    assert_balanced(SHARED_CLONE, DEBUG_AND_RELEASE, "true 3\ntrue 3\n");
}

/// A drop in a cleanup pad that panics: the second panic aborts instead of unwinding through the
/// frame whose cleanup is running (which would drop again what it is dropping).
#[cfg(not(windows))]
const PANIC_IN_CLEANUP: &str = r#"
import { catchVeltPanic } from "velt/zz_unwind";

class Bomb {
  [Symbol.dispose]() {
    panic("in dispose");
  }
}

function inner(xs: number[]): number {
  const b = new Bomb();
  return xs[7];
}

function main() {
  const m = catchVeltPanic(() => { inner([1]); });
  console.log("not reached", m);
}
"#;

#[cfg(not(windows))]
#[test]
fn a_panic_during_a_cleanup_aborts() {
    let dir = test_dir::TestDir::new();
    for args in [&["run"][..], &["run", "--release"][..]] {
        let out = velt(&dir, PANIC_IN_CLEANUP, args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = format!("{args:?}: {:?}\n{stdout}{stderr}", out.status);
        assert!(!stdout.contains("not reached"), "{why}");
        assert!(stderr.contains("panic: in dispose"), "{why}");
        assert!(stderr.contains("panic while unwinding: aborting"), "{why}");
        assert!(!out.status.success(), "{why}");
    }
}

/// A program that doesn't catch has no unwind edges; one that catches has them where frames own
/// values, on the Itanium targets only: Windows (no structured exceptions yet) and WebAssembly
/// get none. Each target is named, so the result doesn't depend on the host running the test.
#[test]
fn only_a_program_that_catches_gets_cleanup_pads() {
    let dir = test_dir::TestDir::new();
    let plain =
        "function f(n: number): string { const s = n.toString() + \"x\"; return g(s) + s; }\n\
                 function g(s: string): string { return s + \"y\"; }\n\
                 console.log(f(3));\n";
    let catching = format!(
        "import {{ catchVeltPanic }} from \"velt/zz_unwind\";\n{plain}\
         console.log(catchVeltPanic(() => {{ console.log(f(4)); }}));\n"
    );
    let targets = [
        ("x86_64-unknown-linux-gnu", true),
        ("aarch64-apple-darwin", true),
        ("x86_64-pc-windows-msvc", false),
        ("aarch64-pc-windows-msvc", false),
        ("wasm32-wasip1", false),
    ];
    for (target, itanium) in targets {
        for (main, catches) in [(plain, false), (catching.as_str(), true)] {
            let out = velt(&dir, main, &["build", "--emit", "vir", "--target", target]);
            let vir = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{target}: {stderr}");
            assert!(vir.contains("fn#"), "{target}: no VIR\n{stderr}");
            let pads = vir.contains(" unwind bb") || vir.contains("velt_eh_resume");
            assert_eq!(
                pads,
                itanium && catches,
                "{target}, catches: {catches}\n{vir}"
            );
        }
    }
}

/// A debug (Cranelift) object with more functions with landing pads than Mach-O has section
/// numbers (255): each function's LSDA had a section of its own, so ld64 crashed reading the
/// object (http-server under `VELT_SCHEDULER=std`, whose catch reaches hundreds of functions with
/// cleanup pads). The LSDAs now share one section; the program links and catches.
#[cfg(not(windows))]
#[test]
fn an_object_with_hundreds_of_functions_with_pads_links() {
    let dir = test_dir::TestDir::new();
    let mut main = String::from("import { catchVeltPanic } from \"velt/zz_unwind\";\n\n");
    main.push_str(
        "function boom(n: number): string {\n  if (n === 299) {\n    const xs = [1];\n    \
         return xs[n].toString();\n  }\n  return n.toString();\n}\n\n",
    );
    for i in 0..300 {
        main.push_str(&format!(
            "function f{i}(n: number): number {{\n  const s = n.toString() + \"v{i}\";\n  \
             const t = boom(n);\n  return s.length + t.length;\n}}\n\n"
        ));
    }
    main.push_str("function main() {\n  let total = 0;\n  const m = catchVeltPanic(() => {\n");
    for i in 0..300 {
        main.push_str(&format!("    total += f{i}({i});\n"));
    }
    main.push_str(
        "  });\n  console.log(total);\n  console.log(m === null ? \"null\" : \"caught\");\n}\n",
    );
    let out = velt(&dir, &main, &["run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{:?}: {stderr}", out.status);
    // f{i}(i) counts i's digits three times and "v": 4 for i < 10, 7 below 100, 10 up to 298
    // (f299's `boom` panics).
    let total = 10 * 4 + 90 * 7 + 199 * 10;
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{total}\ncaught\n"),
        "{stderr}"
    );
}
