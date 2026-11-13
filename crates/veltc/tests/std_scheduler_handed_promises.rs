//! Started promises handed to another task in the executor in Velt (std/lang/hooks/sched_node.velt,
//! #2881, #2891): a promise made by `Promise.withResolvers` starts on the task that made it, and a
//! task it is passed to awaits or drops it, maybe on another worker while the first task finishes
//! it. The finish and the await meet only in the node's flags and awaiter word, which must be
//! claimed atomically, or the awaiting task is never woken. Each program runs several times at 2
//! workers with the checking allocator and the block count (`VELT_RT_DEBUG_ALLOC=1`,
//! `VELT_RC_STATS=1`): the result printed, every block freed.

use std::path::{Path, PathBuf};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Workers only start where `#sched-thread` has threads (macOS, Linux and Windows).
fn has_threads() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux", windows))
}

/// `src` built with `VELT_SCHEDULER=std` in `dir`: the executable.
fn build_std(dir: &Path, name: &str, src: &str) -> PathBuf {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!(
        "{name}_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    exe
}

/// Runs `exe` with `threads` workers (the default rescue), the checking allocator and the block
/// count: its exit code, stdout and stderr.
fn run(exe: &Path, threads: &str) -> (i32, String, String) {
    let o = command(exe)
        .env("VELT_THREADS", threads)
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env("VELT_RC_STATS", "1")
        .env_remove("VELT_RESCUE")
        .env_remove("VELT_SCHEDULER")
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Every block allocated was freed (`VELT_RC_STATS`'s `blocks=allocated/freed`).
fn all_freed(err: &str) -> bool {
    err.split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))
        .and_then(|b| b.split_once('/'))
        .is_some_and(|(a, f)| a == f)
}

/// Runs of [`RESOLVES`]: on `main` before the fix, from 1 run in 3 (a loaded arm64 Mac) to 9 in
/// 10 (x86 Linux) lost the wake at 2 workers; 20 runs leave a broken std about a 0.1% chance to
/// pass them all on the slower of those.
const RUNS: usize = 20;

/// A round's poll opens 100k latches, among them the first and second promise of 1000 waiter
/// tasks, and spins until a task spawned behind it ran: at 2 workers the stall rescue hands that
/// task over, and the waiters, woken by the poll's microtasks finishing their first promise, run
/// on the other worker and await their second promise while the poll's task finishes it. Before
/// the fix, a waiter could read the node's flags before the finish set FINISHED and store its
/// waker after the finish found the awaiter word empty: never woken, so `Promise.all` and `main`
/// waited for ever, and the program ended through the end scan with exit code 0, no output and
/// about 14,000 blocks unfreed (#2891).
#[test]
fn a_waiter_awaiting_a_promise_finished_on_another_worker_is_woken() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "resolves", RESOLVES);
    let mut bad = Vec::new();
    for i in 0..RUNS {
        let (code, out, err) = run(&exe, "2");
        if code != 0 || out != "waited 9990000, marked 10\n" || !all_freed(&err) {
            bad.push(format!(
                "run {i}: exit {code}, stdout {out:?}, stderr {err}"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {RUNS} runs went wrong:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// The program of [`a_waiter_awaiting_a_promise_finished_on_another_worker_is_woken`] (#2888's
/// `a_million_resolves_in_one_poll_are_rescued_mid_loop`).
const RESOLVES: &str = r#"const MARKED = new Atomic<i64>(0);

async function marker(): Promise<void> {
  MARKED.add(1);
}

async function waiter(a: Promise<i64>, b: Promise<i64>): Promise<i64> {
  const x = await a;
  return x + (await b);
}

async function round(r: i64): Promise<i64> {
  const waiters: Promise<i64>[] = [];
  const firsts: ((v: i64) => void)[] = [];
  const seconds: ((v: i64) => void)[] = [];
  for (let i = 0; i < 1000; i++) {
    const a = Promise.withResolvers<i64>();
    const b = Promise.withResolvers<i64>();
    firsts.push(a.resolve);
    seconds.push(b.resolve);
    waiters.push(spawn(waiter(a.promise, b.promise)));
  }
  // The waiters wait.
  await yieldNow();
  const m = spawn(marker());
  let k: i64 = 0;
  for (let i = 0; i < 100000; i++) {
    if (i % 50 == 0) {
      // The first promise of waiter j, then (50 openings later) its second.
      const j = k >> 1;
      if ((k & 1) == 0) {
        const f = firsts[j];
        f(j);
      } else {
        const f = seconds[j];
        f(j);
      }
      k++;
    } else {
      const p = Promise.withResolvers<i64>();
      p.resolve(i);
    }
  }
  while (MARKED.load() <= r) {}
  await m;
  let sum: i64 = 0;
  for (const v of await Promise.all(waiters)) {
    sum += v;
  }
  return sum;
}

async function main() {
  let sum: i64 = 0;
  for (let r = 0; r < 10; r++) {
    sum += await round(r);
    // The spares go idle and retire; worker 0 becomes lazy again.
    await sleep(40);
  }
  console.log(`waited ${sum}, marked ${MARKED.load()}`);
}
"#;
