//! Velt functions C calls (`@export("c_name")`, #2203 step 5, docs/internals/design/c-ffi.md
//! "Exported functions"): a package's C `sources` call them by name, start a thread on one with
//! `pthread_create`, and a panic in one ends the process without unwinding into C, even with a
//! `catchPanic` up the stack. These tests need `VELT_ZIG` as tests/c_sources.rs's do, and run on
//! Unix: Windows links a package's DLL in every build, which can't call into the executable.
//! The white-box test std/ffi/export.test.velt covers calls through `exportedAddress` (`qsort`).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
#[allow(dead_code)]
mod reload_support;
#[allow(dead_code)]
mod runtime_support;
mod test_dir;
mod zig_env;

const HEADER: &str = "#include <stdint.h>\n\nint32_t cb_apply(int32_t x);\nint32_t cb_apply_big(int32_t x);\nint32_t cb_dirty(int32_t bits, int32_t which);\nint64_t cb_run_thread(int64_t n);\n";

/// C calling two exported functions: `cb_twice` directly, `cb_worker` as a thread's start.
const SOURCE: &str = r#"#include "cb.h"
#include <pthread.h>

int32_t cb_twice(int32_t x);
int32_t cb_big(int32_t x);
void *cb_worker(void *arg);

int32_t cb_apply(int32_t x) { return cb_twice(x) + 1; }

int32_t cb_apply_big(int32_t x) { return cb_big(x) + 1; }

int32_t cb_widen_i8(int8_t x);
int32_t cb_widen_u16(uint16_t x);
int32_t cb_bool_i32(_Bool b);

/* Calls an export taking a small integer with `bits` in the whole register, as a caller under
   AAPCS64 outside Darwin may leave it (clang doesn't extend there): the export must extend. */
int32_t cb_dirty(int32_t bits, int32_t which) {
  typedef int32_t (*wide)(int32_t);
  wide f = which == 0 ? (wide)cb_widen_i8 : which == 1 ? (wide)cb_widen_u16 : (wide)cb_bool_i32;
  return f(bits);
}

int64_t cb_run_thread(int64_t n) {
  pthread_t t;
  void *r = 0;
  if (pthread_create(&t, 0, cb_worker, (void *)(intptr_t)n) != 0) return -1;
  pthread_join(t, &r);
  return (int64_t)(intptr_t)r;
}
"#;

const LIB: &str = r#"import * as ffi from "velt/ffi";
import { Ptr, export as cExport } from "velt/ffi";

declare c("cb") function cb_apply(x: i32): i32;
declare c("cb") function cb_apply_big(x: i32): i32;
declare c("cb") function cb_run_thread(n: i64): i64;
declare c("cb") function cb_dirty(bits: i32, which: i32): i32;

@ffi.export("cb_widen_i8")
function widenI8(x: i8): i32 {
  return x as i32;
}

@ffi.export("cb_widen_u16")
function widenU16(x: u16): i32 {
  return x as i32;
}

@ffi.export("cb_bool_i32")
function boolI32(b: boolean): i32 {
  return b ? 1 : 0;
}

export function dirty(bits: i32, which: i32): i32 {
  // SAFETY: it calls back into Velt with a number.
  return unsafe { cb_dirty(bits, which) };
}

@ffi.export("cb_twice")
function twice(x: i32): i32 {
  if (x < 0) {
    panic(`twice of ${x}`);
  }
  return x * 2;
}

// Too large for LLVM to inline into its export, and called from Velt too: without a landing pad
// the export compiled to a jump to it (a tail call), leaving no frame of its own to stop at.
@ffi.export("cb_big")
function big(x: i32): i32 {
  if (x < 0) {
    panic(`big of ${x}`);
  }
  const words: string[] = [];
  let total: i32 = 0;
  for (let i: i32 = 0; i < x; i++) {
    words.push(`w${i * 7 + x}`);
    total += i * 3 + x;
  }
  for (const w of words) {
    if (w.startsWith("w1")) {
      total += 5;
    } else if (w.endsWith("9")) {
      total -= 2;
    }
  }
  const joined = words.join("-");
  if (joined.includes("w10")) {
    total += 11;
  }
  const parts = joined.split("-");
  if (parts.length > 2) {
    total += 1;
  }
  return total;
}

@cExport("cb_worker")
function worker(arg: Ptr<void> | null): Ptr<void> | null {
  const n = arg === null ? 0 : Ptr.addressOf(arg);
  let sum: usize = 0;
  for (let i: usize = 1; i <= n; i++) {
    sum += i;
  }
  const parts: string[] = ["worker ", `${n}`];
  console.log(parts.join(""));
  // SAFETY: C only reads the address back as a number.
  return unsafe { Ptr.at<void>(sum) };
}

export function apply(x: i32): i32 {
  // SAFETY: it reads only its argument, and calls back into Velt.
  return unsafe { cb_apply(x) };
}

export function applyBig(x: i32): i32 {
  // SAFETY: it reads only its argument, and calls back into Velt.
  return unsafe { cb_apply_big(x) };
}

export function bigOf(x: i32): i32 {
  return big(x);
}

export function runThread(n: i64): i64 {
  // SAFETY: it starts a thread on `cb_worker` with `n` as its argument and joins it.
  return unsafe { cb_run_thread(n) };
}
"#;

/// `dir/cb`, its C library calling back, and `dir/app` (`main`), which allows it.
fn project(dir: &Path, main: &str) -> PathBuf {
    let pkg = dir.join("cb");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(pkg.join("native")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"cb\", version: \"0.1.0\", native: { c: { cb: { sources: [\"native/cb.c\"], headers: [\"native/cb.h\"] } } } };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), LIB).unwrap();
    std::fs::write(pkg.join("native/cb.h"), HEADER).unwrap();
    std::fs::write(pkg.join("native/cb.c"), SOURCE).unwrap();
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"cb\"],\n  dependencies: { cb: { path: \"../cb\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(app.join("src/main.velt"), main).unwrap();
    app
}

fn velt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// A home whose zig cache is shared across tests, as tests/c_sources.rs's.
fn home(dir: &Path) -> PathBuf {
    let home = dir.join("home");
    let shared = std::env::temp_dir().join("velt-test-c-sources-zig-cache");
    std::fs::create_dir_all(&shared).unwrap();
    let cache = home.join("cache").join("zig");
    std::fs::create_dir_all(&cache).unwrap();
    let _ = std::os::unix::fs::symlink(&shared, cache.join("build-cache"));
    home
}

/// The builds each test runs: release links the library's static archive, whose references to
/// the program's exports the linker resolves. Debug links its shared library, which finds them
/// in the executable when it loads; that is checked on Linux (macOS debug links of packages
/// with C `sources` are checked by tests/c_sources.rs).
fn modes() -> Vec<&'static [&'static str]> {
    let mut modes: Vec<&[&str]> = vec![&["run", "--release"]];
    if cfg!(target_os = "linux") {
        modes.push(&["run"]);
    }
    modes
}

const MAIN: &str = r#"import { apply, runThread } from "cb";

function main() {
  console.log(apply(20));
  console.log(runThread(10));
}
"#;

#[test]
fn c_calls_exported_functions_by_name_and_starts_a_thread_on_one() {
    let Some(_zig) = zig_env::zig("build C sources calling exported functions") else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home(dir.path());
    let app = project(dir.path(), MAIN);
    for args in modes() {
        let o = velt(&app, &home, args);
        assert!(o.status.success(), "velt {args:?}:\n{}", text(&o));
        // `cb_twice(20) + 1`, and the worker thread's sum of 1..=10, which it prints too: the
        // threads' output may interleave in either order.
        let stdout = String::from_utf8_lossy(&o.stdout);
        let mut lines: Vec<&str> = stdout.lines().collect();
        lines.sort_unstable();
        assert_eq!(
            lines,
            ["41", "55", "worker 10"],
            "velt {args:?}:\n{}",
            text(&o)
        );
    }
}

#[test]
fn a_panic_in_an_exported_function_ends_the_process() {
    let Some(_zig) = zig_env::zig("build C sources calling exported functions") else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home(dir.path());
    let main = "import { apply } from \"cb\";\n\nfunction main() {\n  console.log(apply(1));\n  console.log(apply(-1));\n  console.log(\"after\");\n}\n";
    let app = project(dir.path(), main);
    for args in modes() {
        let o = velt(&app, &home, args);
        assert_eq!(o.status.code(), Some(101), "velt {args:?}:\n{}", text(&o));
        assert_eq!(String::from_utf8_lossy(&o.stdout), "3\n", "{}", text(&o));
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("panic: twice of -1"),
            "{}",
            text(&o)
        );
    }
}

/// `main` calling `call` (of package `cb`) under `catchPanic`.
fn catching_main(call: &str) -> String {
    format!(
        r#"import {{ catchPanic }} from "velt/task";
import {{ apply, applyBig, bigOf }} from "cb";

async function viaC(): Promise<i32> {{
  return {call};
}}

async function main() {{
  console.log(bigOf(3) > 0);
  try {{
    console.log(await catchPanic(viaC));
  }} catch (e) {{
    console.log("caught");
  }}
  console.log("after");
}}
"#
    )
}

/// A `catchPanic` up the stack would stop a panic by unwinding to it, through the C function
/// between: the process aborts instead (owner decision (b) on #2649). The release builds (LLVM)
/// stop the unwind in a landing pad that calls `abort`, which holds with debug info (`-g`, which
/// gives the export unwind information) and when the exported function isn't inlined into its
/// export (`big`, which without the landing pad was reached by a jump); debug builds (Cranelift)
/// give the export no unwind information.
#[test]
fn a_panic_does_not_unwind_into_c_even_when_something_would_catch_it() {
    let Some(_zig) = zig_env::zig("build C sources calling exported functions") else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home(dir.path());
    let mut modes = modes();
    modes.push(&["run", "--release", "-g"]);
    for (call, message) in [
        ("apply(-1)", "panic: twice of -1"),
        ("applyBig(-1)", "panic: big of -1"),
    ] {
        let app = project(dir.path(), &catching_main(call));
        for args in &modes {
            let o = velt(&app, &home, args);
            let out = text(&o);
            assert!(!o.status.success(), "{call}, velt {args:?}:\n{out}");
            assert_eq!(
                String::from_utf8_lossy(&o.stdout),
                "true\n",
                "{call}, velt {args:?}:\n{out}"
            );
            assert!(
                out.contains(message) && !out.contains("caught"),
                "{call}, velt {args:?}:\n{out}"
            );
        }
    }
}

/// `@export` is raw native API (c-ffi.md "The trust boundary"): a package without native code
/// can't define a C symbol, which could replace one of the C library's (`getaddrinfo`).
#[test]
fn only_an_allowed_native_package_exports_to_c() {
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let pkg = dir.path().join("plain");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"plain\", version: \"0.1.0\" };\n",
    )
    .unwrap();
    std::fs::write(
        pkg.join("src/lib.velt"),
        "import * as ffi from \"velt/ffi\";\n\n@ffi.export(\"getaddrinfo\")\nfunction lookup(a: usize, b: usize, c: usize, d: usize): i32 {\n  return -2;\n}\n\nexport function one(): i32 {\n  return 1;\n}\n",
    )
    .unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  dependencies: { plain: { path: \"../plain\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/main.velt"),
        "import { one } from \"plain\";\n\nfunction main() {\n  console.log(one());\n}\n",
    )
    .unwrap();
    let o = velt(&app, &home, &["check"]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(out.contains("`ffi.export` is raw native API"), "{out}");
    assert!(out.contains("package `plain` has no native code"), "{out}");
    // The allowed native package of the other tests checks.
    let app = project(&dir.path().join("allowed"), MAIN);
    let o = velt(&app, &home, &["check"]);
    assert!(o.status.success(), "{}", text(&o));
}

/// An export's `i8`, `u16` and `bool` parameters with garbage in the register's upper bits, as
/// a C caller on aarch64 Linux may pass them: the export extends them itself (LLVM: no
/// `signext`/`zeroext` on its parameters; Cranelift's functions extend already).
#[test]
fn an_export_extends_its_small_integer_parameters_itself() {
    let Some(_zig) = zig_env::zig("build C sources calling exported functions") else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home(dir.path());
    let main = "import { dirty } from \"cb\";\n\nfunction main() {\n  console.log(dirty(0x1234ff80, 0), dirty(0x7fff0001, 1), dirty(0x100, 2));\n}\n";
    let app = project(dir.path(), main);
    for args in modes() {
        let o = velt(&app, &home, args);
        assert!(o.status.success(), "velt {args:?}:\n{}", text(&o));
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "-128 1 0\n",
            "velt {args:?}:\n{}",
            text(&o)
        );
    }
}
