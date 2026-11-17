//! Async code on a C library's thread (#2196): an exported function that a C thread calls keeps
//! a promise of an async call, sets Node timers, clears one, makes a sleep and a `Promise.all`.
//! None of it may touch a worker's own structures from that thread: it all runs on the executor
//! in Velt, handed to worker 0. Called from a module initializer's thread (before the executor
//! started), and from two threads at once while `main` runs, with every worker, one, and a lazy
//! worker 0 (`VELT_RESCUE=1`). Needs `VELT_ZIG` as tests/c_export.rs does, and runs on Unix
//! (Windows links a package's DLL, which can't call into the executable).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod no_window;
#[allow(dead_code)]
mod reload_support;
#[allow(dead_code)]
mod runtime_support;
mod test_dir;
mod zig_env;

const HEADER: &str =
    "#include <stdint.h>\n\nint64_t cbasync_run(int64_t n);\nvoid cbasync_nap(int64_t ms);\n";

/// Starts `n` threads (at most 4) that call the export `cbasync_worker` together, and joins them.
const SOURCE: &str = r#"#include "cbasync.h"
#include <pthread.h>
#include <time.h>

void *cbasync_worker(void *arg);

void cbasync_nap(int64_t ms) {
  struct timespec ts = {ms / 1000, (ms % 1000) * 1000000};
  nanosleep(&ts, 0);
}

static int go;

static void *start(void *arg) {
  while (!__atomic_load_n(&go, __ATOMIC_ACQUIRE)) {
  }
  return cbasync_worker(arg);
}

int64_t cbasync_run(int64_t n) {
  pthread_t t[4];
  int64_t made = 0;
  if (n > 4) n = 4;
  __atomic_store_n(&go, 0, __ATOMIC_RELEASE);
  for (int64_t i = 0; i < n; i++) {
    if (pthread_create(&t[i], 0, start, 0) != 0) break;
    made++;
  }
  __atomic_store_n(&go, 1, __ATOMIC_RELEASE);
  for (int64_t i = 0; i < made; i++) pthread_join(t[i], 0);
  return made;
}
"#;

const LIB: &str = r#"import { Ptr, export as cExport } from "velt/ffi";

declare c("cbasync") function cbasync_run(n: i64): i64;
declare c("cbasync") function cbasync_nap(ms: i64): void;

async function later(tag: string, ms: i64): Promise<i64> {
  await sleep(ms);
  console.log(`kept ${tag}`);
  return 1;
}

async function report(all: Promise<i64[]>): Promise<void> {
  const r = await all;
  console.log(`all ${r.length}`);
}

async function after(s: Promise<void>): Promise<void> {
  await s;
  console.log("slept");
}

/** Runs on a C library's thread. */
@cExport("cbasync_worker")
function worker(arg: Ptr<void> | null): Ptr<void> | null {
  // Kept, not awaited: it starts, and runs on the executor.
  const p = later("call", 5);
  setTimeout(async () => {
    console.log("timeout fired");
  }, 5);
  // Cleared at once: it neither runs nor keeps the program waiting for its time.
  clearTimeout(setTimeout(async () => {
    console.log("cleared timeout ran");
  }, 100000));
  // Cleared once worker 0 put it in Node's order (its task ran meanwhile): worker 0 clears it.
  const late = setTimeout(async () => {
    console.log("late cleared timeout ran");
  }, 100000);
  // SAFETY: it sleeps this thread.
  unsafe {
    cbasync_nap(30);
  }
  clearTimeout(late);
  // A combinator made on this thread, and a sleep, each awaited by a task.
  spawn(report(Promise.all([later("all a", 10), later("all b", 1)])));
  spawn(after(sleep(15)));
  return arg;
}

export function runOn(n: i64): i64 {
  // SAFETY: it starts `n` threads on `cbasync_worker` and joins them.
  return unsafe { cbasync_run(n) };
}

// The module's initializer: a C thread runs it all before `velt_main` started the executor.
runOn(1);
"#;

const MAIN: &str = r#"import { runOn } from "cbasync";

async function main() {
  await yieldNow();
  console.log("main");
  // Two C threads at once while the executor runs.
  console.log(`threads ${runOn(2)}`);
  await sleep(5);
  console.log("main slept");
}
"#;

/// `dir/cbasync`, its C library, and `dir/app` (`main`), which allows it.
fn project(dir: &Path) -> PathBuf {
    let pkg = dir.join("cbasync");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(pkg.join("native")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"cbasync\", version: \"0.1.0\", native: { c: { cbasync: { sources: [\"native/cbasync.c\"], headers: [\"native/cbasync.h\"] } } } };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), LIB).unwrap();
    std::fs::write(pkg.join("native/cbasync.h"), HEADER).unwrap();
    std::fs::write(pkg.join("native/cbasync.c"), SOURCE).unwrap();
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"cbasync\"],\n  dependencies: { cbasync: { path: \"../cbasync\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(app.join("src/main.velt"), MAIN).unwrap();
    app
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

/// Runs `velt args` in `dir`, killed after a minute (a hang fails the test instead of the run):
/// its exit code (`None` when killed) and output.
fn velt(dir: &Path, home: &Path, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .env_remove("VELT_THREADS")
        .env_remove("VELT_RESCUE")
        .envs(env.iter().copied())
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let o = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (code, text)
}

#[test]
fn async_code_on_c_threads_runs_on_the_executor() {
    let Some(_zig) = zig_env::zig("build C sources calling exported functions") else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home(dir.path());
    let app = project(dir.path());
    // Every worker, one worker, and a lazy worker 0 the monitor may rescue.
    let modes: [&[(&str, &str)]; 3] = [
        &[],
        &[("VELT_THREADS", "1")],
        &[("VELT_THREADS", "1"), ("VELT_RESCUE", "1")],
    ];
    for env in modes {
        let (code, out) = velt(&app, &home, &["run", "--release"], env);
        assert_eq!(code, Some(0), "{env:?}: {out}");
        let lines: Vec<&str> = out.lines().filter(|l| !l.starts_with("warning:")).collect();
        let count = |l: &str| lines.iter().filter(|x| **x == l).count();
        // Three calls: one from the initializer's thread, two from threads during `main`. Each
        // one's promises, timer, combinator and sleep finish; the program waits for them.
        for want in [
            "kept call",
            "timeout fired",
            "kept all a",
            "kept all b",
            "all 2",
            "slept",
        ] {
            assert_eq!(count(want), 3, "{env:?} {want:?}: {out}");
        }
        assert_eq!(count("cleared timeout ran"), 0, "{env:?}: {out}");
        assert_eq!(count("late cleared timeout ran"), 0, "{env:?}: {out}");
        for want in ["main", "threads 2", "main slept"] {
            assert_eq!(count(want), 1, "{env:?} {want:?}: {out}");
        }
    }
}
