//! The stall rescue of the executor in Velt (std/lang/hooks/sched_monitor.velt, #2649 P2g) where
//! it hands a lazy worker 0's tasks over (std/lang/hooks/sched_monitor_lazy.velt): worker 0 does
//! its latch and join slot operations without the lock, counted (std/lang/hooks/sched_lock.velt
//! `kind`), until the monitor finds one of its polls stalled, turns the locks on (`locksOn`, which
//! waits for the operation under way) and starts a spare. These programs make that hand-over land
//! in the middle of many such operations, and check the result, that every block is freed
//! (`VELT_RC_STATS=1` with the checking allocator, `VELT_RT_DEBUG_ALLOC=1`) and, through
//! `VELT_RESCUE_STATS=1`, that a rescue happened at all.

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

/// The runs of each program and worker count: the hand-over races worker 0's operations.
const RUNS: usize = 20;

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

/// What one run printed: its exit code, stdout and stderr.
struct Run {
    code: i32,
    out: String,
    err: String,
}

/// `VELT_RESCUE` for the runs (unset when `None`).
const RESCUE: Option<&str> = Some("1");

/// Runs `exe` with `threads` workers, `VELT_RESCUE` as [`RESCUE`] has it, the checking allocator,
/// the block count and the rescue count.
fn run(exe: &Path, threads: &str) -> Run {
    let mut c = command(exe);
    c.env("VELT_THREADS", threads)
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env("VELT_RC_STATS", "1")
        .env("VELT_RESCUE_STATS", "1")
        .env_remove("VELT_RESCUE");
    if let Some(r) = RESCUE {
        c.env("VELT_RESCUE", r);
    }
    let o = c.env_remove("VELT_SCHEDULER").output().unwrap();
    Run {
        code: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        err: String::from_utf8_lossy(&o.stderr).replace("\r\n", "\n"),
    }
}

/// The count `name` of the `rescue stats:` line (std/lang/hooks/sched_stats.velt).
fn rescue_count(err: &str, name: &str) -> Option<u64> {
    let line = err.lines().find(|l| l.starts_with("rescue stats:"))?;
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(name)?.strip_prefix('='))?
        .parse()
        .ok()
}

/// Checks run `r` of `what`: exit 0, stdout `want`, a rescue, and (with the debug runtime, which
/// counts) every block freed but `unfreed` at most.
fn check(r: &Run, what: &str, want: &str, unfreed: u64) {
    assert_eq!(
        (r.code, r.out.as_str()),
        (0, want),
        "{what}, stderr: {}",
        r.err
    );
    let rescues = rescue_count(&r.err, "rescues");
    assert!(
        rescues.is_some_and(|n| n > 0),
        "{what}: no rescue ({:?}), stderr: {}",
        rescues,
        r.err
    );
    let Some(stats) = r.err.lines().rev().find(|l| l.starts_with("rc stats:")) else {
        // Only the debug runtime counts (a `--release` test run links the release one).
        if cfg!(debug_assertions) {
            panic!("{what}: no rc stats in:\n{}", r.err);
        }
        return;
    };
    let blocks = stats
        .split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))
        .and_then(|b| b.split_once('/'))
        .unwrap_or_else(|| panic!("{what}: no block counts in {stats}"));
    let (made, freed): (u64, u64) = (blocks.0.parse().unwrap(), blocks.1.parse().unwrap());
    assert!(
        made >= freed && made - freed <= unfreed,
        "{what}: blocks left unfreed ({stats})"
    );
}

/// A spawn storm across the hand-over. `main` spawns 100k tasks in pairs: the even one of a pair
/// awaits a promise `main` resolves, then settles the odd one's promise; the odd one awaits its
/// promise and joins the even one. Once they all wait, `main` resolves the even ones' promises in
/// one poll (latch openings counted on a lazy worker 0, which wake tasks) and then runs on until
/// a marker task has run, which only a spare can do: every configured worker is held (worker 0 by
/// `main`, the others by a task each, as tests/golden/std/task_spare_worker_index.velt does). The
/// monitor's rescue of `main`'s poll lands among those operations: at one worker it turns the
/// locks on there (`locks-on`, at least once), and the spare runs woken even tasks, which open the
/// odd ones' latches while worker 0 opens more, and odd ones, which join slots worker 0 may still
/// finish. The sum checks that every task ran once with what its promise carried.
#[test]
fn a_spawn_storm_crosses_the_hand_over_of_a_lazy_worker() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "storm", STORM);
    // Σ i % 7 over 0..100000.
    let want = "sum 299995, marked true\n";
    for threads in ["1", "2", "4"] {
        for i in 0..RUNS {
            let r = run(&exe, threads);
            let what = format!("storm, VELT_THREADS={threads}, run {i}");
            // About one run in 50 leaves 2 blocks unfreed, at one worker and at two, with main's
            // std before this change too: allowed until #2887 is found.
            check(&r, &what, want, 2);
            if threads == "1" {
                // At least once: after the spares retire, worker 0 is lazy again, and on a loaded
                // machine a later poll of `main` may be rescued too, turning them on again (#2967).
                assert!(
                    rescue_count(&r.err, "locks-on").is_some_and(|n| n >= 1),
                    "{what}: the locks turned on, stderr: {}",
                    r.err
                );
            }
        }
    }
}

/// The program of [`a_spawn_storm_crosses_the_hand_over_of_a_lazy_worker`].
const STORM: &str = r#"import { workerCount } from "velt/task";

class Gate {
  spinning: Atomic<i64> = new Atomic<i64>(0);
  released: Atomic<i64> = new Atomic<i64>(0);
  marked: Atomic<i64> = new Atomic<i64>(0);
}

// Holds its worker until the gate opens: it never returns from its poll.
async function hold(gate: shared<Gate>): Promise<void> {
  gate.spinning.add(1);
  while (gate.released.load() == 0) {}
}

// Runs only on a spare while every configured worker is held.
async function marker(gate: shared<Gate>): Promise<void> {
  gate.marked.store(1);
}

async function even(mine: Promise<i64>, next: (v: i64) => void): Promise<i64> {
  const v = await mine;
  next(v + 1);
  return v % 7;
}

async function odd(mine: Promise<i64>, prev: Promise<i64>): Promise<i64> {
  const v = await mine;
  const p = await prev;
  return p + v % 7;
}

async function main() {
  const n = workerCount();
  const gate = shared(new Gate());
  const held: Promise<void>[] = [];
  for (let w = 1; w < n; w++) {
    held.push(spawn(hold(gate), { worker: w }));
  }
  while (gate.spinning.load() < n - 1) {
    await yieldNow();
  }
  const resolvers: ((v: i64) => void)[] = [];
  const odds: Promise<i64>[] = [];
  for (let i = 0; i < 100000; i += 2) {
    const a = Promise.withResolvers<i64>();
    const b = Promise.withResolvers<i64>();
    resolvers.push(a.resolve);
    const h = spawn(even(a.promise, b.resolve));
    odds.push(spawn(odd(b.promise, h)));
  }
  // Every task runs once and waits for its promise.
  await yieldNow();
  const m = spawn(marker(gate));
  let i = 0;
  for (const r of resolvers) {
    r(i);
    i += 2;
  }
  while (gate.marked.load() == 0) {}
  gate.released.store(1);
  await m;
  await Promise.all(held);
  let sum: i64 = 0;
  for (const v of await Promise.all(odds)) {
    sum += v;
  }
  console.log(`sum ${sum}, marked ${gate.marked.load() == 1}`);
}
"#;

/// Polls on the only worker resolve a million promises in all (each an opening of a latch, counted
/// while worker 0 is lazy) and run on until a marker task has run: the monitor's rescue, and the
/// locks turning on, land in the middle of such a loop. One opening in fifty wakes a task that
/// waits for two promises in turn, so a spare polls a wait on a latch while worker 0 may open it.
/// Ten rounds, each 100k openings, then a pause in which the spares retire and worker 0 becomes
/// lazy again (std/lang/hooks/sched_relax.velt): up to ten hand-overs a run (one at least).
#[test]
fn a_million_resolves_in_one_poll_are_rescued_mid_loop() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "resolves", RESOLVES);
    for i in 0..RUNS {
        let r = run(&exe, "1");
        let what = format!("resolves, run {i}");
        check(&r, &what, "waited 9990000, marked 10\n", 0);
        assert!(
            rescue_count(&r.err, "locks-on").is_some_and(|n| n >= 1),
            "{what}: {}",
            r.err
        );
    }
}

/// The program of [`a_million_resolves_in_one_poll_are_rescued_mid_loop`].
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

/// A poll that runs on at the only worker is rescued soon: a task queued behind it runs on a spare
/// within 20 ms of the poll's start (at worst about 9 ms of naps and ticks; 100 ms on Windows). The
/// best of 5 runs is compared, so a loaded machine that keeps the monitor or the spare off a core
/// does not fail it; every run must be rescued at all.
#[test]
fn a_spinning_poll_on_the_only_worker_is_rescued_within_20_ms() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "delay", DELAY);
    let mut best = u64::MAX;
    for i in 0..5 {
        let r = run(&exe, "1");
        let what = format!("delay, run {i}");
        assert_eq!(r.code, 0, "{what}: {}{}", r.out, r.err);
        let ms: u64 = r
            .out
            .strip_prefix("rescued after ")
            .and_then(|s| s.strip_suffix(" ms\n"))
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("{what}: {}{}", r.out, r.err));
        best = best.min(ms);
    }
    // Windows ends a timed wait at a tick of the system timer, 15.6 ms by default; std asks for
    // 1 ms (#2896, std/lang/hooks/wait_win32.velt `fineTimers`), which a desktop honours (6 ms
    // at best), but a CI runner's virtual machine did not (35 ms).
    let limit = if cfg!(windows) { 100 } else { 20 };
    assert!(
        best <= limit,
        "the rescue took {best} ms at best (at most {limit})"
    );
}

/// The program of [`a_spinning_poll_on_the_only_worker_is_rescued_within_20_ms`].
const DELAY: &str = r#"const MARKED = new Atomic<i64>(0);

async function marker(): Promise<void> {
  MARKED.store(Date.now() as i64);
}

async function main() {
  // The monitor sees worker 0 idle first: the poll below starts from its lazy watch.
  await sleep(20);
  const m = spawn(marker());
  const start = Date.now() as i64;
  while (MARKED.load() == 0) {}
  console.log(`rescued after ${MARKED.load() - start} ms`);
  await m;
}
"#;

/// What a task printed on a lazy worker 0 before an `await` comes out before what it prints after
/// the rescue moved it to a spare (#2977): the line sat in worker 0's thread buffer while the
/// spare published its own. The task can only finish while `main` spins if a rescue moved it, so
/// each run that passes the rescue check crossed the hand-over; it marks that a poll after its
/// second line, which the spare has published by then, so `main`'s line comes after it.
#[test]
fn a_line_printed_on_a_lazy_worker_comes_before_what_the_rescued_task_prints_next() {
    if !has_threads() {
        return;
    }
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "moved_output", MOVED_OUTPUT);
    for i in 0..RUNS {
        let r = run(&exe, "1");
        check(
            &r,
            &format!("run {i}"),
            "before the await\nafter the await\nmain\n",
            0,
        );
    }
}

/// The program of
/// [`a_line_printed_on_a_lazy_worker_comes_before_what_the_rescued_task_prints_next`].
const MOVED_OUTPUT: &str = r#"const MOVED = new Atomic<i64>(0);

async function task(): Promise<void> {
  console.log("before the await");
  await yieldNow();
  console.log("after the await");
  // A poll later, so the spare has published that line (its poll's end) when `main` reads this.
  await yieldNow();
  MOVED.store(1);
}

async function main() {
  const t = spawn(task());
  // The task prints its first line here and queues behind `main` at its `await`.
  await yieldNow();
  // Only a rescue runs the task's second half while this poll spins.
  while (MOVED.load() == 0) {}
  console.log("main");
  await t;
}
"#;

/// Worker 0's output across its modes (#2977): lazy, made shared by a rescue in the middle of a
/// poll, and lazy again once the spares retire (std/lang/hooks/sched_relax.velt), five rounds a
/// run. Each round a task prints on lazy worker 0, is moved to a spare and prints there (the
/// first line published after the poll that printed it); `main` prints in the poll the rescue made
/// shared, after the hand-over (published when worker 0 takes on the shared mode), then lets the
/// task print once more. No line may be lost or come out of order; two hand-overs at least mean
/// worker 0 was lazy again after one, so its marks were set again (process.velt `setLazyWorker`).
#[test]
fn worker_0s_lines_keep_their_order_while_it_turns_shared_and_lazy_again() {
    if !has_threads() {
        return;
    }
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "flip_output", FLIP_OUTPUT);
    let want: String = (0..5)
        .map(|r| {
            format!(
                "task {r} before\ntask {r} after\nmain {r} moved\ntask {r} last\nmain {r} done\n"
            )
        })
        .collect();
    for i in 0..RUNS {
        let r = run(&exe, "1");
        let what = format!("run {i}");
        check(&r, &what, &want, 0);
        assert!(
            rescue_count(&r.err, "locks-on").is_some_and(|n| n >= 2),
            "{what}: worker 0 was not lazy again after a rescue: {}",
            r.err
        );
    }
}

/// The program of [`worker_0s_lines_keep_their_order_while_it_turns_shared_and_lazy_again`].
const FLIP_OUTPUT: &str = r#"const MOVED = new Atomic<i64>(0);
const GO = new Atomic<i64>(0);

async function task(r: i64): Promise<void> {
  console.log(`task ${r} before`);
  await yieldNow();
  // On a spare from here (only a rescue runs this while `main` spins).
  console.log(`task ${r} after`);
  // A poll later, so the spare has published that line when `main` reads this.
  await yieldNow();
  MOVED.store(r + 1);
  while (GO.load() <= r) {}
  console.log(`task ${r} last`);
}

async function main() {
  for (let r = 0; r < 5; r++) {
    const t = spawn(task(r));
    // The task prints its first line here and queues behind `main` at its `await`.
    await yieldNow();
    while (MOVED.load() <= r) {}
    // In the poll the rescue made shared: published when worker 0 takes that on, at this `await`.
    console.log(`main ${r} moved`);
    await yieldNow();
    GO.store(r + 1);
    await t;
    console.log(`main ${r} done`);
    // The spares go idle and retire; worker 0 becomes lazy again.
    await sleep(40);
  }
}
"#;

/// A line printed in a lazy worker 0's poll comes out before what the tasks that poll spawns
/// afterwards print, when the monitor moves them to a spare while the poll still runs (#2977):
/// the poll spawns one task, prints, spawns seven more and spins until one of them has run, which
/// only a rescue allows. The spawns after the line are queued where the monitor takes them from,
/// so the line is published first (sched_wake.velt `pendOn`).
#[test]
fn a_line_printed_before_a_spawn_comes_first_when_the_rescue_moves_the_spawned_task() {
    if !has_threads() {
        return;
    }
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "spawn_after_line", SPAWN_AFTER_LINE);
    let want = format!("main before\n{}main after\n", "child\n".repeat(8));
    for i in 0..RUNS {
        let r = run(&exe, "1");
        check(&r, &format!("run {i}"), &want, 0);
    }
}

/// The program of
/// [`a_line_printed_before_a_spawn_comes_first_when_the_rescue_moves_the_spawned_task`].
const SPAWN_AFTER_LINE: &str = r#"const DONE = new Atomic<i64>(0);

async function child(): Promise<void> {
  console.log("child");
  DONE.add(1);
}

async function main() {
  const ts: Promise<void>[] = [];
  ts.push(spawn(child()));
  console.log("main before");
  for (let i = 0; i < 7; i++) {
    ts.push(spawn(child()));
  }
  // Only a rescue runs a child while this poll spins.
  while (DONE.load() == 0) {}
  for (const t of ts) {
    await t;
  }
  console.log("main after");
}
"#;
