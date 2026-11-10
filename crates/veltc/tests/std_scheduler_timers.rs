//! Timers under the executor in Velt (std/lang/hooks/sched_sleep.velt, sched_order.velt, #2649):
//! `sleep` resumes in timer order (deadline, then creation) whoever polls it, and `setTimeout` /
//! `setInterval` / `setImmediate` callbacks run in Node's order (one list per delay, the lists by
//! expiry and id, the check phase for immediates, each callback's microtasks before the next).
//! Each program is built with and without `VELT_SCHEDULER=std`, both binaries run with
//! `VELT_THREADS=1` and must print the same; only the one built with it holds the executor.
//! Delays are longer than the busy waits before them, so a loaded machine cannot change the order.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

/// `src` built (with `VELT_SCHEDULER=std` when `std`) and run, which must exit with `code`: its
/// stdout, and its binary.
fn build_and_run(dir: &Path, name: &str, src: &str, std: bool, code: i32) -> (String, Vec<u8>) {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let exe = dir.join(format!("{name}{}{suffix}", if std { "_std" } else { "" }));
    let mut build = command(env!("CARGO_BIN_EXE_velt"));
    build
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env_remove("VELT_SCHEDULER");
    if std {
        build.env("VELT_SCHEDULER", "std");
    }
    let o = build.output().unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{name}: {stderr}");
    let o = command(&exe)
        .env("VELT_THREADS", "1")
        .env_remove("VELT_SCHEDULER")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(
        o.status.code(),
        Some(code),
        "{name} (std: {std}): {out}{stderr}"
    );
    (out, std::fs::read(&exe).unwrap())
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Builds and runs `src` both ways, each exiting with `code`: each prints `want`, and only the
/// std build has the executor's timers.
fn same_on_both(name: &str, src: &str, want: &str, code: i32) {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let (rt, rt_bin) = build_and_run(dir.path(), name, src, false, code);
    let (std, std_bin) = build_and_run(dir.path(), name, src, true, code);
    assert_eq!(rt, want, "{name}: the runtime's executor");
    assert_eq!(std, want, "{name}: the executor in Velt");
    let hook = if src.contains("sleep(") {
        "schedSleep"
    } else {
        "schedTimerTurn"
    };
    assert!(contains(&std_bin, hook.as_bytes()), "{name}: no {hook}");
    assert!(
        !contains(&rt_bin, hook.as_bytes()),
        "{name}: {hook} without the variable"
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
    same_on_both(
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
    same_on_both(
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
    same_on_both(
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
    same_on_both(
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
    same_on_both(
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
    same_on_both(
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
    same_on_both(
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
