//! A promise settled from a thread that runs no task (#3390): a C library's thread calls an
//! exported function that resolves or rejects a promise `main` awaits, with an object holding a
//! promise of its own, and `main` hands the object on to another task the moment it has it.
//! std's `resolver` / `rejecter` (std/lang/promise/settle.velt) settle such a call at once. They
//! used to open the promise's latch while their frame still held the transferred object: handed
//! on in that window the object was copied, the copy shared the promise inside on `main`'s task,
//! and the frame's own reference was then dropped on the C thread (#3360's failure, on the path
//! #3369 left as it was). Needs `VELT_ZIG` as tests/c_export.rs does, and runs on Unix (Windows
//! links a package's DLL, which can't call into the executable).

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

const HEADER: &str = "#include <stdint.h>\n\nvoid cbres_start(void);\nint64_t cbres_done(void);\n";

/// Starts one detached thread that calls the export `cbres_settle` once, then sets `cbres_done`.
const SOURCE: &str = r#"#include "cbres.h"
#include <pthread.h>

void cbres_settle(void);

static int64_t done;

int64_t cbres_done(void) { return __atomic_load_n(&done, __ATOMIC_ACQUIRE); }

static void *settler(void *arg) {
  cbres_settle();
  __atomic_store_n(&done, 1, __ATOMIC_RELEASE);
  return arg;
}

void cbres_start(void) {
  pthread_t t;
  __atomic_store_n(&done, 0, __ATOMIC_RELEASE);
  if (pthread_create(&t, 0, settler, 0) == 0) pthread_detach(t);
  else __atomic_store_n(&done, 1, __ATOMIC_RELEASE);
}
"#;

const LIB: &str = r#"import { export as cExport } from "velt/ffi";

declare c("cbres") function cbres_start(): void;
declare c("cbres") function cbres_done(): i64;

export class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

// Finishes after `n` turns of its task.
async function soon(n: i64): Promise<number> {
  for (let i: i64 = 0; i < n; i++) {
    await yieldNow();
  }
  return 1;
}

// What the C thread's call settles: the promise `main` awaits, through its `resolve` or its
// `reject` (one of the two is set), with an object holding a promise of the call's own.
let resolveWith: ((h: Holder) => void) | null = null;
let rejectWith: ((h: Holder) => void) | null = null;
let round: i64 = 0;

/** Runs on a C library's thread, which runs no task. */
@cExport("cbres_settle")
function settle(): void {
  const h = new Holder(soon(1 + (round % 4)));
  const resolve = resolveWith;
  const reject = rejectWith;
  if (resolve !== null) {
    resolve(h);
  } else if (reject !== null) {
    reject(h);
  }
}

/** Starts a C thread that settles the promise of `resolve` or `reject` with a holder. */
export function settleFromC(
  r: i64,
  resolve: ((h: Holder) => void) | null,
  reject: ((h: Holder) => void) | null,
): void {
  round = r;
  resolveWith = resolve;
  rejectWith = reject;
  // SAFETY: it starts a detached thread that calls this module's export once.
  unsafe {
    cbres_start();
  }
}

/** Has the C thread's call returned? Then it is done with the settler. */
export function settled(): bool {
  // SAFETY: an atomic load in C.
  return unsafe { cbres_done() } !== 0;
}

/** Lets go of the settlers once the call has returned. */
export function forget(): void {
  resolveWith = null;
  rejectWith = null;
}
"#;

const MAIN: &str = r#"import { forget, Holder, settled, settleFromC } from "cbres";

// On worker 2: awaits the promise in what it was given.
async function user(h: Holder): Promise<number> {
  return await h.p;
}

async function main() {
  let sum = 0;
  for (let r: i64 = 0; r < ROUNDS; r++) {
    if (r % 2 == 0) {
      const got = Promise.withResolvers<Holder>();
      settleFromC(r, got.resolve, null);
      // Handed on the moment it is settled: main is awake, and the C thread may still be in the
      // call that settled it.
      sum += await spawn(user(await got.promise), { worker: 2 });
    } else {
      const got = Promise.withResolvers<number, Holder>();
      settleFromC(r, null, got.reject);
      try {
        await got.promise;
      } catch (h) {
        sum += await spawn(user(h), { worker: 2 });
      }
    }
    while (!settled()) {
      await yieldNow();
    }
    forget();
  }
  console.log(`done ${sum}`);
}
"#;

/// Rounds of [`MAIN`]: half resolved, half rejected from a C thread.
const ROUNDS: u32 = 200;

/// Runs at each worker count.
const RUNS: usize = 5;

/// `dir/cbres`, its C library, and `dir/app` (`main`), which allows it.
fn project(dir: &Path) -> PathBuf {
    let pkg = dir.join("cbres");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(pkg.join("native")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"cbres\", version: \"0.1.0\", native: { c: { cbres: { sources: [\"native/cbres.c\"], headers: [\"native/cbres.h\"] } } } };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), LIB).unwrap();
    std::fs::write(pkg.join("native/cbres.h"), HEADER).unwrap();
    std::fs::write(pkg.join("native/cbres.c"), SOURCE).unwrap();
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"cbres\"],\n  dependencies: { cbres: { path: \"../cbres\" } },\n};\n",
    )
    .unwrap();
    let main = MAIN.replace("ROUNDS", &ROUNDS.to_string());
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

/// Runs `velt args` in `dir`, killed after ten minutes for a `build` and one for a run (a hang
/// fails the test instead of the run): its exit code (`None` when killed), stdout and stderr.
fn velt(
    dir: &Path,
    home: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> (Option<i32>, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .env_remove("VELT_THREADS")
        .env_remove("VELT_RESCUE")
        .env("VELT_SCHEDULER", "std")
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
    (
        code,
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Whether the block counters' report says every block allocated was freed.
fn all_freed(err: &str) -> bool {
    err.split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))
        .and_then(|b| b.split_once('/'))
        .is_some_and(|(a, f)| a == f)
}

/// The window is the few instructions between the latch's opening and the end of the settling
/// call's frame, against the awaiter's wake-up on its own worker, so `main`'s std passes this
/// most runs; with the window widened (the settling call spinning after it opens the latch)
/// `main`'s std fails and the fix doesn't.
#[test]
fn a_promise_settled_from_a_c_thread_lets_go_before_its_awaiter_sees_it() {
    let Some(_zig) = zig_env::zig("build C sources calling exported functions") else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home(dir.path());
    let app = project(dir.path());
    // Built first, outside the runs' limit: each run below finds the build current.
    let (code, out, err) = velt(&app, &home, &["build"], &[]);
    assert_eq!(code, Some(0), "{out}{err}");
    let want = format!("done {ROUNDS}\n");
    let mut bad = Vec::new();
    for threads in ["4", "8"] {
        for i in 0..RUNS {
            let env = [
                ("VELT_THREADS", threads),
                ("VELT_RT_DEBUG_ALLOC", "1"),
                ("VELT_RC_STATS", "1"),
            ];
            let (code, out, err) = velt(&app, &home, &["run"], &env);
            let out: String = out
                .lines()
                .filter(|l| !l.starts_with("warning:"))
                .map(|l| format!("{l}\n"))
                .collect();
            if code != Some(0) || out != want || !all_freed(&err) {
                bad.push(format!(
                    "{threads} workers, run {i}: exit {code:?}, stdout {out:?}, stderr {err}"
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} runs went wrong:\n{}",
        bad.len(),
        bad.join("\n")
    );
}
