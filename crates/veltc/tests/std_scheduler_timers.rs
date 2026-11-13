//! Timers under the executor in Velt (std/lang/hooks/sched_sleep.velt, sched_order.velt, #2649):
//! `sleep` resumes in timer order (deadline, then creation) whoever polls it, and `setTimeout` /
//! `setInterval` / `setImmediate` callbacks run in Node's order (one list per delay, the lists by
//! expiry and id, the check phase for immediates, each callback's microtasks before the next).
//! Each program is built with `VELT_SCHEDULER=std`, runs with `VELT_THREADS=1` and must print
//! what Node prints, with the executor's timers in its binary.
//! Delays are longer than the busy waits before them, so a loaded machine cannot change the order.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

/// `src` built with `VELT_SCHEDULER=std` and run with `VELT_THREADS=1`, which must exit with
/// `code`: its stdout, and its binary.
fn build_and_run(dir: &Path, name: &str, src: &str, code: i32) -> (String, Vec<u8>) {
    build_and_run_on(dir, name, src, code, "1")
}

/// [`build_and_run`] with `VELT_THREADS=threads`.
fn build_and_run_on(
    dir: &Path,
    name: &str,
    src: &str,
    code: i32,
    threads: &str,
) -> (String, Vec<u8>) {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let exe = dir.join(format!("{name}_std{suffix}"));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{name}: {stderr}");
    let out = run_exe(&exe, threads, code, name);
    (out, std::fs::read(&exe).unwrap())
}

/// Runs `exe` with `VELT_THREADS=threads`, which must exit with `code` within a minute: its stdout.
fn run_exe(exe: &Path, threads: &str, code: i32, what: &str) -> String {
    let mut child = command(exe)
        .env("VELT_THREADS", threads)
        .env_remove("VELT_SCHEDULER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A watchdog, not a synchronization: a timer that never fires would hang the test run.
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(60) {
            child.kill().unwrap();
            panic!("{what}: still running after a minute");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = child.wait_with_output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(code), "{what}: {out}{stderr}");
    out
}

/// Whether a `VELT_SCHEDULER=std` program builds here: the executor in Velt waits in an epoll,
/// kqueue or (Windows) AFD reactor; other OSes have none.
fn std_builds_here() -> bool {
    cfg!(any(unix, windows))
}

/// What marks std function `function` in a built program: its name, or on Windows, where a PE
/// file names no functions and a debug build's PDB only the exported ones, the panic locations
/// its file's code carries (`std/lang/hooks/sched.velt:line:col`).
fn marker(function: &str) -> String {
    if cfg!(windows) {
        "std/lang/hooks/sched.velt:".to_string()
    } else {
        function.to_string()
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Builds and runs `src`, exiting with `code`: it prints `want`, and has the executor's timers.
fn prints(name: &str, src: &str, want: &str, code: i32) {
    if !std_builds_here() {
        return;
    }
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let (std, std_bin) = build_and_run(dir.path(), name, src, code);
    assert_eq!(std, want, "{name}");
    let hook = if src.contains("sleep(") {
        "schedSleep"
    } else {
        "schedTimerTurn"
    };
    assert!(
        contains(&std_bin, marker(hook).as_bytes()),
        "{name}: no {hook}"
    );
}

const BUSY: &str = "function busy(ms: number) {
  const s = Date.now();
  while (Date.now() - s < ms) {}
}
";

/// One list per delay: a timer joins its delay's list (`C` after `A`, before the later list of
/// `B`), and the lists run by expiry, not by when they were made (`t 10` before `t 30`).
#[test]
fn timer_lists_run_by_expiry_then_list_id() {
    prints(
        "timer_lists",
        &format!(
            "{BUSY}
function main() {{
  setTimeout(() => console.log(\"A 10\"), 10);
  setTimeout(() => console.log(\"B 12\"), 12);
  busy(5);
  setTimeout(() => console.log(\"C 10\"), 10);
  busy(20);
  setTimeout(() => console.log(\"t 90\"), 90);
  setTimeout(() => console.log(\"t 70\"), 70);
  setTimeout(() => console.log(\"t 50\"), 50);
  console.log(\"main end\");
}}
"
        ),
        "main end\nA 10\nC 10\nB 12\nt 50\nt 70\nt 90\n",
        0,
    );
}

/// Immediates run in the check phase: after the timers due when the loop turns (those created
/// during the pass too), and before a timeout created next to them.
#[test]
fn immediates_run_after_the_timers_pass() {
    prints(
        "timer_check_phase",
        &format!(
            "{BUSY}
function main() {{
  setTimeout(() => console.log(\"t1\"), 10);
  setImmediate(() => console.log(\"main immediate\"));
  setTimeout(() => {{
    console.log(\"outer\");
    setTimeout(() => console.log(\"nested timeout\"), 0);
    setImmediate(() => console.log(\"nested immediate\"));
    queueMicrotask(() => console.log(\"nested microtask\"));
  }}, 20);
  busy(30);
  console.log(\"main end\");
}}
"
        ),
        "main end\nt1\nouter\nnested microtask\nmain immediate\nnested immediate\nnested timeout\n",
        0,
    );
}

/// A callback clears a timer due with it (it never runs) and an interval due with it; each
/// callback's microtasks run before the next callback.
#[test]
fn a_callback_clears_timers_due_with_it() {
    prints(
        "timer_clear_inside",
        "function main() {
  for (const n of [\"a\", \"b\"]) {
    setTimeout(() => {
      console.log(\"timeout\", n);
      queueMicrotask(() => console.log(\"microtask\", n));
    }, 5);
  }
  let victim: Timer | null = null;
  setTimeout(() => {
    console.log(\"clearing\");
    if (victim != null) {
      clearTimeout(victim);
    }
  }, 15);
  victim = setTimeout(() => console.log(\"cleared timer ran\"), 15);
  const every = setInterval(() => console.log(\"interval\"), 25);
  setTimeout(() => {
    console.log(\"stopping interval\");
    clearInterval(every);
  }, 25);
  setTimeout(() => console.log(\"last\"), 60);
}
",
        "timeout a\nmicrotask a\ntimeout b\nmicrotask b\nclearing\ninterval\nstopping interval\nlast\n",
        0,
    );
}

/// An interval runs on its schedule while an earlier run's promise is pending: the runs
/// interleave at their `await`s.
#[test]
fn interval_runs_overlap() {
    prints(
        "timer_interval_overlap",
        "async function main() {
  const runs = shared(0);
  const { promise, resolve } = Promise.withResolvers<void>();
  const t = setInterval(
    async () => {
      const k = runs.add(1);
      if (k > 2) {
        return;
      }
      console.log(\"start\", k);
      while (runs.get() < 2) {
        await sleep(1);
      }
      console.log(\"end\", k);
      if (k == 1) {
        resolve();
      }
    },
    5,
  );
  await promise;
  t.clear();
  console.log(\"cleared\");
}
",
        "start 1\nstart 2\nend 2\nend 1\ncleared\n",
        0,
    );
}

/// An `unref()`'d timer does not keep the process alive: it runs while a ref'd one does, and the
/// program ends without the one still pending then.
#[test]
fn an_unrefd_timer_lets_the_process_end() {
    prints(
        "timer_unref",
        "function main(): void {
  setTimeout(() => console.log(\"never\"), 100000).unref();
  setTimeout(() => console.log(\"unref'd\"), 1).unref();
  setTimeout(() => console.log(\"ref'd\"), 20);
  console.log(\"main end\");
}
",
        "main end\nunref'd\nref'd\n",
        0,
    );
}

/// An error a timer's callback throws ends the process at once, before the microtasks it queued
/// and the timers after it.
#[test]
fn an_error_in_a_callback_ends_the_process_at_once() {
    prints(
        "timer_uncaught",
        "setTimeout(
  () => {
    console.log(\"t\");
    queueMicrotask(() => console.log(\"after-throw micro\"));
    throw new RangeError(\"t failed\");
  },
  1,
);
setTimeout(() => console.log(\"t2\"), 3);
",
        "t\n",
        1,
    );
}

/// Sleeps due together resume in timer order, whoever polls them: `Promise.all` polls `a` first,
/// whose timer comes second; a sleep created first but polled later keeps its place; and a
/// promise that yields after its sleep lets the next due sleep fire in the same poll; the
/// microtasks a resumed sleeper queued run before the next sleep fires.
#[test]
fn sleeps_resume_in_timer_order_whoever_polls_them() {
    prints(
        "sleep_order",
        &format!(
            "{BUSY}
async function step(name: string, ms: i64): Promise<string> {{
  await sleep(ms);
  console.log(name);
  return name;
}}

async function withYield(): Promise<string> {{
  await sleep(0);
  console.log(\"first\");
  await yieldNow();
  console.log(\"first after yieldNow\");
  return \"first\";
}}

async function withMicrotask(): Promise<string> {{
  await sleep(0);
  queueMicrotask(() => console.log(\"microtask of the first\"));
  console.log(\"first\");
  await Promise.resolve();
  console.log(\"first, one step on\");
  return \"first\";
}}

async function main() {{
  const m1 = withMicrotask();
  const m2 = step(\"second\", 30);
  busy(60);
  console.log(await Promise.all([m1, m2]));
  const b = step(\"b\", 0);
  const a = step(\"a\", 30);
  busy(60);
  console.log(await Promise.all([a, b]));
  const y1 = withYield();
  const y2 = step(\"second\", 30);
  busy(60);
  console.log(await Promise.all([y1, y2]));
  const mine = sleep(5);
  const theirs = step(\"theirs\", 5);
  busy(20);
  await mine;
  console.log(\"main\");
  await theirs;
}}
"
        ),
        "first\nmicrotask of the first\nfirst, one step on\nsecond\n[ 'first', 'second' ]\n\
b\na\n[ 'a', 'b' ]\nfirst\nsecond\nfirst after yieldNow\n[ 'first', 'second' ]\nmain\ntheirs\n",
        0,
    );
}

/// A sleep and a Node timer due together run in timer order across both kinds: the sleep due at 2
/// ms before the timeout due at 10, which Node prints too (`sleep = ms => new
/// Promise(r => setTimeout(r, ms))`: `sleep 2|timeout 10|spawned sleep 2|timeout 8`). The rest
/// differs from Node: the spawned task's body runs once `main` lets go, so with one worker its
/// sleep is created after `busy` and comes due after the timeout of 8 (Node runs the body before
/// `busy`; #1332's case). The executor in Velt keeps timer order there (one worker has no stall
/// rescue unless `VELT_RESCUE=1`).
#[test]
fn sleeps_and_timers_due_together_run_in_one_order() {
    if !std_builds_here() {
        return;
    }
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let src = format!(
        "{BUSY}
async function main() {{
  setTimeout(() => console.log(\"timeout 10\"), 10);
  const s = sleep(2);
  busy(30);
  await s;
  console.log(\"sleep 2\");
  const p = spawn(async () => {{
    await sleep(2);
    console.log(\"spawned sleep 2\");
    return 0;
  }});
  setTimeout(() => console.log(\"timeout 8\"), 8);
  busy(30);
  await p;
  await sleep(20);
}}
"
    );
    let (std, std_bin) = build_and_run(dir.path(), "sleep_timer", &src, 0);
    assert_eq!(
        std, "sleep 2\ntimeout 10\ntimeout 8\nspawned sleep 2\n",
        "the executor in Velt"
    );
    assert!(contains(&std_bin, marker("schedSleep").as_bytes()));
}

/// A task pinned to a worker of the runtime runs its timers there: the callback that resolves its
/// promise, and one nothing awaits, both run (std: the runtime boxes and starts the callbacks'
/// promises off the worker, sched_off.velt), and the worker's timer prints between them and the
/// pinned task's sleep (std: the worker writes its stdout out before it waits). Two threads, 20
/// runs.
#[test]
fn a_pinned_spawn_runs_its_timers() {
    if !std_builds_here() {
        return;
    }
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let src = "async function pinned(): Promise<i64> {
  await new Promise<void>((resolve) => setTimeout(() => resolve(), 5));
  console.log(\"pinned timeout fired\");
  setTimeout(() => console.log(\"pinned fire-and-forget\"), 5);
  await sleep(600);
  console.log(\"pinned slept 600\");
  return 2;
}

async function main() {
  setTimeout(() => console.log(\"worker timeout 60\"), 60);
  const n = await spawn(pinned(), { worker: 1 });
  console.log(n);
}
";
    let want =
        "pinned timeout fired\npinned fire-and-forget\nworker timeout 60\npinned slept 600\n2\n";
    let (out, _) = build_and_run_on(dir.path(), "pinned_timer", src, 0, "2");
    assert_eq!(out, want);
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let exe = dir.path().join(format!("pinned_timer_std{suffix}"));
    for run in 0..19 {
        let out = run_exe(&exe, "2", 0, &format!("run {run}"));
        assert_eq!(out, want, "run {run}");
    }
}

/// What a program printed reaches a pipe before its worker sleeps (#2873): a program that prints
/// a line and then sleeps for ten minutes shows the line at once, as the runtime's executor does
/// when its workers go idle, not when the process ends. Run at one worker: with more, a parking
/// worker already flushed, and the bug was the one-worker gap. Read with a watchdog, not a timing:
/// the line comes within a minute or the test fails.
#[test]
fn output_reaches_a_pipe_before_the_worker_sleeps() {
    use std::io::{BufRead, BufReader};
    if !std_builds_here() {
        return;
    }
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("forever.velt");
    let src = "async function main() {\n  console.log(\"started\");\n  await sleep(600000);\n}\n";
    std::fs::write(&file, src).unwrap();
    let exe = dir.path().join(if cfg!(windows) {
        "forever.exe"
    } else {
        "forever"
    });
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut child = command(&exe)
        .env("VELT_THREADS", "1")
        .env_remove("VELT_SCHEDULER")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(out).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(60));
    child.kill().unwrap();
    child.wait().unwrap();
    let line = line.expect("the line printed before the sleep did not reach the pipe");
    assert_eq!(line.trim(), "started");
}

/// The programs of Node timers under tasks on several workers under the executor in Velt: one
/// order for the process whichever worker sets or steps it, and a busy task's timers held back
/// without holding back the others'. Two are goldens (tests/golden/std, run by the golden suite
/// with the runtime's executor too); `timer_order_tasks` is not (std_timer_programs/), since the
/// runtime's executor keeps no order for the process. Built once each, run five times with
/// `VELT_THREADS=4` and with 2.
#[test]
fn timer_goldens_run_in_node_order_on_several_workers() {
    if !std_builds_here() {
        return;
    }
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let golden = manifest.join("../../tests/golden/std");
    let programs = manifest.join("tests/std_timer_programs");
    for (dir_of, name) in [
        (&programs, "timer_order_tasks"),
        (&golden, "timer_spawned_busy"),
        (&golden, "timer_spawned_busy_first"),
    ] {
        let src = std::fs::read_to_string(dir_of.join(format!("{name}.velt"))).unwrap();
        let want = std::fs::read_to_string(dir_of.join(format!("{name}.out")))
            .unwrap()
            .replace("\r\n", "\n");
        let (out, _) = build_and_run_on(dir.path(), name, &src, 0, "4");
        assert_eq!(out, want, "{name}, 4 workers");
        let exe = dir.path().join(format!("{name}_std"));
        for threads in ["4", "2"] {
            for run in 0..5 {
                let what = format!("{name}, {threads} workers, run {run}");
                assert_eq!(run_exe(&exe, threads, 0, &what), want, "{what}");
            }
        }
    }
}

/// A timer set by a poll that ends at once fires while the worker that ran it spins in the next
/// task: the end of that poll looks at the order (a worker woken to look while it ran found its
/// task busy, and parked without a time), and one parked worker waits for the timer's time. The
/// spin gives up after 3 s; the timer is due after 20 ms. Five runs with `VELT_THREADS=4` and 2.
#[test]
fn a_timer_set_just_before_its_worker_spins_fires_meanwhile() {
    if !std_builds_here() {
        return;
    }
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let src = "class State {
  fired: boolean;
  constructor() {
    this.fired = false;
  }
}

async function main() {
  const state = shared(new Mutex<State>(new State()));
  const s2 = state.clone();
  const s3 = state.clone();
  setTimeout(() => {
    s3.with((v) => {
      v.fired = true;
    });
  }, 20);
  // Still in this poll when another worker looks at the order: the timer's task is busy then,
  // and only the end of this poll can hand its turn on.
  const t0 = Date.now();
  while (Date.now() - t0 < 50) {}
  const spin = spawn(async (): Promise<number> => {
    const s = Date.now();
    while (!s2.with((v) => v.fired) && Date.now() - s < 3000) {}
    return Date.now() - s;
  });
  const took = await spin;
  console.log(took < 1000 ? \"fired during the spin\" : `late: ${took} ms`);
}
";
    let want = "fired during the spin\n";
    let (out, _) = build_and_run_on(dir.path(), "busy_end", src, 0, "4");
    assert_eq!(out, want);
    let exe = dir.path().join("busy_end_std");
    for threads in ["4", "2"] {
        for run in 0..5 {
            let what = format!("{threads} workers, run {run}");
            assert_eq!(run_exe(&exe, threads, 0, &what), want, "{what}");
        }
    }
}
