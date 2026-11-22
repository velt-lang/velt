//! A `Mutex` that threads wait for (#2196, std/lang/hooks/mutex.velt): std's inline lock takes
//! an uncontended lock with one compare-and-swap and calls the runtime's slow path when the lock
//! is held (it spins, then parks the thread: velt_rt `mutex.rs`), and its unlock calls the
//! runtime when threads are parked on it, which wakes one. Each program runs counted
//! (`VELT_RC_STATS=1`, `VELT_RT_DEBUG_ALLOC=1`) at 4 and 8 workers: exit 0, its output, every
//! block freed. The programs pin tasks to workers 1 and 2, so not at one worker.

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

/// Every block allocated was freed (`VELT_RC_STATS`'s `blocks=allocated/freed`).
fn all_freed(err: &str) -> bool {
    err.split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))
        .and_then(|b| b.split_once('/'))
        .is_some_and(|(a, f)| a == f)
}

/// Builds `src` and runs it `runs` times at 4 and at 8 workers, counted: exit 0, `want` printed,
/// every block freed.
fn check(name: &str, src: &str, want: &str, runs: usize) {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), name, src);
    let mut bad = Vec::new();
    for threads in ["4", "8"] {
        for i in 0..runs {
            let o = command(&exe)
                .env("VELT_THREADS", threads)
                .env("VELT_RT_DEBUG_ALLOC", "1")
                .env("VELT_RC_STATS", "1")
                .env_remove("VELT_RESCUE")
                .env_remove("VELT_SCHEDULER")
                .output()
                .unwrap();
            let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
            let err = String::from_utf8_lossy(&o.stderr);
            if o.status.code() != Some(0) || out != want || !all_freed(&err) {
                bad.push(format!(
                    "{threads} workers, run {i}: exit {:?}, stdout {out:?}, stderr {err}",
                    o.status.code()
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{name}: {} runs went wrong:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// A task on worker 1 holds the lock until a task on worker 2 is about to lock, then long enough
/// for it to park; the unlock wakes it, and four tasks then add under the lock from both workers.
/// An unlock that ignored a parked thread would leave the waiter parked for ever.
#[test]
fn a_thread_parked_on_a_mutex_is_woken_by_its_unlock() {
    check(
        "mutex_parked",
        MUTEX_PARKED,
        "waiter saw 11 true\ntotal 200011\n",
        3,
    );
}

/// The program of [`a_thread_parked_on_a_mutex_is_woken_by_its_unlock`].
const MUTEX_PARKED: &str = r#"// A `Mutex` a thread parks on: a task on worker 1 takes the lock and keeps it until a task on
// worker 2 says it is about to lock, then a while longer, so that the second spins, parks (the
// runtime's slow path: velt_rt `mutex.rs`) and is woken by the unlock. Then both add under the
// lock, many times, from four tasks: the total counts every one.
const READY = new Atomic<i64>(0);
const WAITING = new Atomic<i64>(0);

class Count {
  n: i64 = 0;
}

async function holder(m: shared<Mutex<Count>>): Promise<i64> {
  return m.with((c) => {
    READY.store(1);
    while (WAITING.load() == 0) {}
    // Long after the other thread's few rounds of spinning: it parks meanwhile.
    let x: i64 = 0;
    for (let i: i64 = 0; i < 3000000; i++) {
      x = (x + i) % 1000003;
    }
    c.n += 1;
    return x;
  });
}

async function waiter(m: shared<Mutex<Count>>): Promise<i64> {
  while (READY.load() == 0) {}
  WAITING.store(1);
  return m.with((c) => {
    c.n += 10;
    return c.n;
  });
}

async function adder(m: shared<Mutex<Count>>, k: i64): Promise<void> {
  for (let i: i64 = 0; i < 20000; i++) {
    m.with((c) => {
      c.n += k;
    });
  }
}

async function main() {
  const m = shared(new Mutex<Count>(new Count()));
  const h = spawn(holder(m), { worker: 1 });
  const w = spawn(waiter(m), { worker: 2 });
  const x = await h;
  console.log("waiter saw", await w, x >= 0);
  const ps: Promise<void>[] = [];
  for (let k: i64 = 1; k <= 4; k++) {
    ps.push(spawn(adder(m, k), { worker: 1 + (k % 2) }));
  }
  for (const p of ps) {
    await p;
  }
  console.log("total", m.with((c) => c.n));
}
"#;
