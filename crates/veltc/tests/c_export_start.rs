//! A C library's thread calling an exported Velt function before anything started the executor
//! in Velt (#3085): the export's `spawn` must not make that thread worker 0. The task it spawns
//! runs once `main`'s thread runs the executor. Needs `VELT_ZIG` as tests/c_export.rs does, and
//! runs on Unix (Windows links a package's DLL, which can't call into the executable).

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
    "#include <stdint.h>\n\nint64_t early_run(void);\nint64_t early_start(void);\n";

/// `early_run` starts a thread on the export `early_worker` and joins it; `early_start` starts
/// one that nothing waits for, which calls the export a moment later.
const SOURCE: &str = r#"#include "early.h"
#include <pthread.h>
#include <unistd.h>

void *early_worker(void *arg);

int64_t early_run(void) {
  pthread_t t;
  void *r = 0;
  if (pthread_create(&t, 0, early_worker, 0) != 0) return -1;
  pthread_join(t, &r);
  return (int64_t)(intptr_t)r;
}

static void *call_later(void *arg) {
  usleep(20000);
  return early_worker(arg);
}

int64_t early_start(void) {
  pthread_t t;
  if (pthread_create(&t, 0, call_later, 0) != 0) return -1;
  pthread_detach(t);
  return 0;
}
"#;

const LIB: &str = r#"import { Ptr, export as cExport } from "velt/ffi";

declare c("early") function early_run(): i64;

// Still pending when `main` returns: the program waits for it, as Node waits for a pending timer.
async function child(): Promise<void> {
  await sleep(20);
  console.log("child ran");
}

/** Runs on the C library's thread: it spawns a task, which the executor runs later. */
@cExport("early_worker")
function worker(arg: Ptr<void> | null): Ptr<void> | null {
  console.log("export on the C thread");
  // A timer from the C thread, cleared at once: it must not need worker 0's block (#3085).
  clearTimeout(setTimeout(() => {}, 1000));
  spawn(child());
  return arg;
}

export function runEarly(): i64 {
  // SAFETY: it starts a thread on `early_worker` and joins it.
  return unsafe { early_run() };
}

// The module's initializer: the C thread spawns before `velt_main` started the executor.
runEarly();
"#;

/// `main`'s thread must still be worker 0: its `sleep` is the executor's (when the C thread had
/// started the executor, `main` ran off every worker and its `sleep` panicked in the runtime).
const MAIN: &str = r#"import { runEarly } from "early";

async function main() {
  console.log(`early ${typeof runEarly}`);
  await yieldNow();
  console.log("main");
  // A C thread calls back while the executor runs: its spawn is handed over the same way.
  runEarly();
  await sleep(5);
  console.log("slept");
}
"#;

/// A promise shared on the C library's thread (#3782): the export keeps a promise in two places.
/// Such a share isn't handed to the executor yet, so it stops the program with a message saying
/// so: never a crash or a hang, and never a share on whichever task worker 0 runs meanwhile.
const SHARE_LIB: &str = r#"import { Ptr, export as cExport } from "velt/ffi";

declare c("early") function early_run(): i64;
declare c("early") function early_start(): i64;

async function later(): Promise<i64> {
  await sleep(1);
  return 7;
}

async function report(p: Promise<i64>, q: Promise<i64>): Promise<void> {
  console.log(`got ${await p} ${await q}`);
}

/** Runs on the C library's thread: `p` and `q` share one promise. */
@cExport("early_worker")
function worker(arg: Ptr<void> | null): Ptr<void> | null {
  const p = later();
  const q = p;
  spawn(report(p, q));
  return arg;
}

export function runEarly(): i64 {
  // SAFETY: it starts a thread on `early_worker` and joins it.
  return unsafe { early_run() };
}

export function startEarly(): i64 {
  // SAFETY: it starts a thread that calls `early_worker`.
  return unsafe { early_start() };
}
"#;

/// The C thread shares while `main`'s task waits in the C call that started it: the task current
/// on worker 0 is not the thread's.
const SHARE_BLOCKED: &str = r#"import { runEarly } from "early";

async function main() {
  await yieldNow();
  console.log("main");
  runEarly();
  await sleep(20);
  console.log("not reached");
}
"#;

/// The C thread shares on its own while `main` goes on and sleeps: whether worker 0 runs a task
/// or none at that moment, the share is refused.
const SHARE_ASLEEP: &str = r#"import { startEarly } from "early";

async function main() {
  console.log("main");
  startEarly();
  await sleep(5000);
  console.log("not reached");
}
"#;

/// The same share in the library's initializer: its C thread runs before the executor started.
const SHARE_BEFORE: &str = r#"import { runEarly } from "early";

async function main() {
  console.log(`not reached ${typeof runEarly}`);
}
"#;

/// `dir/early`, its C library, and `dir/app` (`main`), which allows it.
fn project(dir: &Path) -> PathBuf {
    project_with(dir, LIB, MAIN)
}

/// [`project`] with the library's `lib` and the app's `main`.
fn project_with(dir: &Path, lib: &str, main: &str) -> PathBuf {
    let pkg = dir.join("early");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(pkg.join("native")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"early\", version: \"0.1.0\", native: { c: { early: { sources: [\"native/early.c\"], headers: [\"native/early.h\"] } } } };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), lib).unwrap();
    std::fs::write(pkg.join("native/early.h"), HEADER).unwrap();
    std::fs::write(pkg.join("native/early.c"), SOURCE).unwrap();
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"early\"],\n  dependencies: { early: { path: \"../early\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(app.join("src/main.velt"), main).unwrap();
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

/// Runs `velt args` in `dir`, killed after a minute (a hang fails the test instead of the run),
/// ten for a `build` (a release build of std with an empty cache took up to a minute on a loaded
/// machine): its exit code (`None` when killed) and output.
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
    let limit = Duration::from_secs(if args.first() == Some(&"build") {
        600
    } else {
        60
    });
    let start = Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if start.elapsed() > limit {
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
fn a_spawn_on_a_c_thread_before_the_executor_started_leaves_main_on_worker_0() {
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
    // Built first, outside the runs' limit: each run below finds the build current.
    let (code, out) = velt(&app, &home, &["build", "--release"], &[]);
    assert_eq!(code, Some(0), "{out}");
    for env in modes {
        let (code, out) = velt(&app, &home, &["run", "--release"], env);
        assert_eq!(code, Some(0), "{env:?}: {out}");
        let lines: Vec<&str> = out.lines().filter(|l| !l.starts_with("warning:")).collect();
        let count = |l: &str| lines.iter().filter(|x| **x == l).count();
        // One spawn from the initializer's C thread (before the executor started), one from a C
        // thread during `main`; both children still sleep when `main` returns, and the program waits.
        assert_eq!(count("export on the C thread"), 2, "{out}");
        assert_eq!(count("child ran"), 2, "{out}");
        for want in ["early function", "main", "slept"] {
            assert_eq!(count(want), 1, "{want:?}: {out}");
        }
        let at = |l: &str| lines.iter().position(|x| *x == l);
        assert!(at("main") < at("slept"), "{out}");
    }
}

#[test]
fn a_promise_shared_on_a_c_thread_stops_the_program_with_its_reason() {
    let Some(_zig) = zig_env::zig("build C sources calling exported functions") else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home(dir.path());
    let in_initializer = format!("{SHARE_LIB}\nrunEarly();\n");
    let cases = [
        ("blocked", SHARE_LIB, SHARE_BLOCKED),
        ("asleep", SHARE_LIB, SHARE_ASLEEP),
        ("before", in_initializer.as_str(), SHARE_BEFORE),
    ];
    for (name, lib, main) in cases {
        let app = project_with(&dir.path().join(name), lib, main);
        let (code, out) = velt(&app, &home, &["build", "--release"], &[]);
        assert_eq!(code, Some(0), "{name}: {out}");
        for env in [&[][..], &[("VELT_THREADS", "1")][..]] {
            let (code, out) = velt(&app, &home, &["run", "--release"], env);
            // Stopped by the panic: not killed by the time limit, not a crash. (What `main`
            // logged before it may still be in its buffer: the panic is another thread's.)
            assert_eq!(code, Some(101), "{name} {env:?}: {out}");
            assert!(
                out.contains("a promise cannot be shared on a C library's own thread yet (#3782)"),
                "{name} {env:?}: {out}"
            );
            assert!(!out.contains("got 7 7"), "{name} {env:?}: {out}");
            assert!(!out.contains("not reached"), "{name} {env:?}: {out}");
        }
    }
}
