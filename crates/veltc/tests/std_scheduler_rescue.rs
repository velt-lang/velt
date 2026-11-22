//! The stall rescue of the executor in Velt (std/lang/hooks/sched_monitor.velt, #2649 P2g) where
//! it hands a lazy worker 0's tasks over (std/lang/hooks/sched_monitor_lazy.velt): worker 0 does
//! its latch and join slot operations without the lock, counted (std/lang/hooks/sched_lock.velt
//! `kind`), until the monitor finds one of its polls stalled, turns the locks on (`locksOn`, which
//! waits for the operation under way) and starts a spare. These programs make that hand-over land
//! in the middle of many such operations, and check the result, that every block is freed
//! (`VELT_RC_STATS=1` with the checking allocator, `VELT_RT_DEBUG_ALLOC=1`) and, through
//! `VELT_RESCUE_STATS=1`, that a rescue happened at all.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

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

/// How long one run may take before it counts as hung: killed, its exit code [`HUNG`].
const DEADLINE: Duration = Duration::from_secs(60);

/// The exit code of a run killed at [`DEADLINE`].
const HUNG: i32 = -2;

/// Runs `exe` with `threads` workers, `VELT_RESCUE` as [`RESCUE`] has it, the checking allocator,
/// the block count and the rescue count; killed at [`DEADLINE`] (a lost wake hangs).
fn run(exe: &Path, threads: &str) -> Run {
    run_with(exe, threads, &[])
}

/// [`run`] with the environment variables `env` set as well.
fn run_with(exe: &Path, threads: &str, env: &[(&str, &str)]) -> Run {
    let mut c = command(exe);
    c.envs(env.iter().copied())
        .env("VELT_THREADS", threads)
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env("VELT_RC_STATS", "1")
        .env("VELT_RESCUE_STATS", "1")
        .env_remove("VELT_RESCUE");
    if let Some(r) = RESCUE {
        c.env("VELT_RESCUE", r);
    }
    let mut child = c
        .env_remove("VELT_SCHEDULER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let out = drain(child.stdout.take().unwrap());
    let err = drain(child.stderr.take().unwrap());
    let start = Instant::now();
    let code = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s.code().unwrap_or(-1);
        }
        if start.elapsed() > DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            break HUNG;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let text = |h: std::thread::JoinHandle<Vec<u8>>| {
        String::from_utf8_lossy(&h.join().unwrap()).replace("\r\n", "\n")
    };
    Run {
        code,
        out: text(out),
        err: text(err),
    }
}

/// Reads `r` to its end on a thread of its own, so a run cannot block on a full pipe.
fn drain(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = r.read_to_end(&mut v);
        v
    })
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
    assert_ne!(
        r.code,
        HUNG,
        "{what}: hung, killed after {} s; stdout: {}stderr: {}",
        DEADLINE.as_secs(),
        r.out,
        r.err
    );
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
            check(&r, &what, want, 0);
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

/// Starting a spare worker's thread counts no block (std/lang/hooks/thread_posix.velt
/// `startThread`): the monitor's thread starts it while the program runs on, so a counted block
/// it freed after `pthread_create` could still be unfreed when the spare had finished the last
/// task and the program took its counts (one run in a few thousand on a loaded Linux machine:
/// `blocks=20/19`). The program needs one spare, or two with `DEEP=1` (the task on the first
/// spare spins until one queued behind it ran, which only a second rescue does); its tasks and
/// what they allocate are the same either way, so the blocks made are too.
#[test]
fn starting_a_spare_counts_no_block() {
    // The threads of thread_posix.velt; Windows' `CreateThread` takes no room of ours.
    if !has_threads() || !cfg!(unix) || !cfg!(debug_assertions) {
        return;
    }
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "spares", SPARES);
    // The spares a run started and the blocks it made.
    let counts = |deep: &str| {
        let what = format!("DEEP={deep}");
        let r = run_with(&exe, "1", &[("DEEP", deep)]);
        check(&r, &what, "done\n", 0);
        let spares = rescue_count(&r.err, "spares").unwrap();
        let stats = r.err.lines().rev().find(|l| l.starts_with("rc stats:"));
        let made = stats
            .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("blocks=")))
            .and_then(|b| b.split_once('/'))
            .and_then(|(made, _)| made.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("{what}: no block counts in {}", r.err));
        (spares, made)
    };
    // On a loaded machine the monitor may start a spare more than a run needs: every run makes
    // the same blocks all the same, and one pair of runs at least differs in its spares.
    let mut differed = false;
    for _ in 0..RUNS {
        let (one, deep) = (counts("0"), counts("1"));
        assert_eq!(
            one.1, deep.1,
            "blocks made with {} and with {} spares",
            one.0, deep.0
        );
        differed |= one.0 != deep.0;
    }
    assert!(differed, "no pair of runs differed in its spares");
}

/// The program of [`starting_a_spare_counts_no_block`].
const SPARES: &str = r#"const A_DONE = new Atomic<i64>(0);
const B_DONE = new Atomic<i64>(0);

async function a(deep: bool): Promise<void> {
  await yieldNow();
  if (deep) {
    // On the spare that took worker 0's queue, with `b` queued behind: only a second rescue,
    // onto a second spare, runs `b` while this poll spins.
    while (B_DONE.load() == 0) {}
  }
  A_DONE.store(1);
}

async function b(): Promise<void> {
  await yieldNow();
  B_DONE.store(1);
}

async function main() {
  const deep = process.env.DEEP === "1";
  const ta = spawn(a(deep));
  const tb = spawn(b());
  await yieldNow();
  // Only a rescue runs `a` while this poll spins.
  while (A_DONE.load() == 0) {}
  await ta;
  await tb;
  console.log("done");
}
"#;

/// Worker 0's output across its modes (#2977): lazy, made shared by a rescue in the middle of a
/// poll, and lazy again once the spares retire (std/lang/hooks/sched_relax.velt), five rounds a
/// run. Each round a task prints on lazy worker 0, is moved to a spare and prints there (the
/// first line published after the poll that printed it); `main` prints in the poll the rescue made
/// shared, after the hand-over (published when worker 0 takes on the shared mode), then lets the
/// task print once more. No line may be lost or come out of order; hand-overs after the first
/// mean worker 0 was lazy again, so its marks were set again (process.velt `setLazyWorker`).
#[test]
fn worker_0s_lines_keep_their_order_while_it_turns_shared_and_lazy_again() {
    flip_rounds(&flip_output(TIMER_WAIT));
}

/// [`worker_0s_lines_keep_their_order_while_it_turns_shared_and_lazy_again`] with `main` waiting
/// between rounds in a `sleep`. A rescue may move `main` to a spare (`lazyRescue` takes the whole
/// queue), and its sleep then anchors it there; asked to retire, the spare carries that sleep with
/// `main` to worker 0 (#3117: before, it declined, and worker 0 was not lazy again, on Windows in
/// most runs).
#[test]
fn worker_0s_lines_keep_their_order_while_it_turns_shared_and_lazy_again_with_sleeps() {
    flip_rounds(&flip_output("sleep(40)"));
}

/// Of five rounds that each start with a rescue of a lazy worker 0, how many must find it lazy
/// (`locks-on`): after the first, worker 0 is lazy again in two of the four pauses at least. Each
/// pause gives the spares time to retire many times over; on a loaded machine one may still miss.
/// The monitor used to take its request to retire back whenever a spare it had just woken to
/// answer was slow to run (a tick; #3148, #3500), so on a machine with fewer cores than busy
/// threads worker 0 was lazy again in one pause of four or none in most runs.
const LAZY_ROUNDS_MIN: u64 = 3;

/// Five rounds of `src` (a [`FLIP_OUTPUT`]): each run's lines in order, and worker 0 lazy again
/// after a rescue in two pauses of four at least ([`LAZY_ROUNDS_MIN`]).
fn flip_rounds(src: &str) {
    if !has_threads() {
        return;
    }
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "flip_output", src);
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
            rescue_count(&r.err, "locks-on").is_some_and(|n| n >= LAZY_ROUNDS_MIN),
            "{what}: worker 0 was lazy again in fewer than two pauses of four: {}",
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
    // The spares go idle and retire; worker 0 becomes lazy again. The wait: a Node timer,
    // which anchors no task, or a `sleep`, which a retiring spare carries (`flip_output`).
    await $WAIT;
  }
}
"#;

/// [`FLIP_OUTPUT`] with `wait` as `main`'s wait between rounds: a Node timer ([`TIMER_WAIT`])
/// leaves `main` movable; a `sleep` anchors it to the spare a rescue moved it to, which carries
/// the sleep to worker 0 when it retires.
fn flip_output(wait: &str) -> String {
    FLIP_OUTPUT.replace("$WAIT", wait)
}

/// The wait of [`worker_0s_lines_keep_their_order_while_it_turns_shared_and_lazy_again`].
const TIMER_WAIT: &str = "new Promise<void>((resolve) => setTimeout(() => resolve(), 40))";

/// A task woken by a spare in the middle of the spare's poll is not run next on that spare (#3060):
/// each round a rescued task resolves what `main` awaits and runs on, then `main` sleeps. Run on
/// the spare, `main`'s sleep went into the spare's timer block, and a spare with a timer declines
/// to retire, so worker 0 never was lazy again (on Windows the race made the worker-0 test above
/// fail 9 runs in 10; this one wakes `main` mid-poll every round, on any OS).
#[test]
fn a_task_woken_by_a_spare_mid_poll_goes_home_so_the_spares_retire() {
    if !has_threads() {
        return;
    }
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "woken_by_spare", WOKEN_BY_SPARE);
    let want: String = (0..5)
        .map(|r| format!("task {r} before\ntask {r} after\nmain {r} moved\nmain {r} woken\n"))
        .collect();
    for i in 0..RUNS {
        let r = run(&exe, "1");
        let what = format!("run {i}");
        check(&r, &what, &want, 0);
        assert!(
            rescue_count(&r.err, "locks-on").is_some_and(|n| n >= LAZY_ROUNDS_MIN),
            "{what}: worker 0 was lazy again in fewer than two pauses of four: {}",
            r.err
        );
    }
}

/// The program of [`a_task_woken_by_a_spare_mid_poll_goes_home_so_the_spares_retire`].
const WOKEN_BY_SPARE: &str = r#"const MOVED = new Atomic<i64>(0);
const GO = new Atomic<i64>(0);

async function task(r: i64, done: (v: i64) => void): Promise<void> {
  console.log(`task ${r} before`);
  await yieldNow();
  // On a spare from here (only a rescue runs this while `main` spins).
  console.log(`task ${r} after`);
  await yieldNow();
  MOVED.store(r + 1);
  while (GO.load() <= r) {}
  // Wakes `main` while this poll still runs, then runs on a little.
  done(r);
  let x = 0;
  for (let i = 0; i < 200000; i++) {
    x += i % 3;
  }
  if (x < 0) {
    console.log("never");
  }
}

async function main() {
  for (let r = 0; r < 5; r++) {
    const { promise, resolve } = Promise.withResolvers<i64>();
    const t = spawn(task(r, resolve));
    await yieldNow();
    while (MOVED.load() <= r) {}
    console.log(`main ${r} moved`);
    await yieldNow();
    GO.store(r + 1);
    await promise;
    console.log(`main ${r} woken`);
    // The spares go idle and retire; worker 0 becomes lazy again.
    await sleep(40);
    await t;
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

/// A spare that waits for the time of Node's timers and is asked to retire passes that wait on
/// (#3060): each round the rescued task ends on the spare after `main` awaited a `setTimeout`
/// promise, while worker 0 still runs `busy`, so the spare parks first and waits for the timer's
/// time (sched_order_wait.velt `O_WAITER`) and worker 0 parks without one. The monitor then asks
/// the spare to retire; retired, it waits without a time too. Before, it gave the role up and
/// nobody waited for the timer: the run hung (every run on macOS).
#[test]
fn a_spare_waiting_for_a_node_timer_hands_the_wait_on_when_it_retires() {
    if !has_threads() {
        return;
    }
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "waiter_retires", WAITER_RETIRES);
    let want: String = (0..3).map(|r| format!("round {r}\n")).collect();
    for i in 0..RUNS {
        let r = run(&exe, "1");
        check(&r, &format!("run {i}"), &want, 0);
    }
}

/// The program of [`a_spare_waiting_for_a_node_timer_hands_the_wait_on_when_it_retires`].
const WAITER_RETIRES: &str = r#"const MOVED = new Atomic<i64>(0);
const GO = new Atomic<i64>(0);

async function task(r: i64): Promise<void> {
  await yieldNow();
  // On a spare from here (only a rescue runs this while `main` spins).
  MOVED.store(r + 1);
  while (GO.load() <= r) {}
}

// Runs on worker 0 after `main` awaits the timer: the task ends and the spare parks meanwhile.
async function busy(r: i64): Promise<void> {
  GO.store(r + 1);
  const until = Date.now() + 5;
  while (Date.now() < until) {}
}

async function main() {
  for (let r = 0; r < 3; r++) {
    const t = spawn(task(r));
    await yieldNow();
    while (MOVED.load() <= r) {}
    const p = new Promise<void>((resolve) => setTimeout(() => resolve(), 100));
    const b = spawn(busy(r));
    await p;
    await t;
    await b;
    console.log(`round ${r}`);
  }
}
"#;

/// A spare asked to retire while a task the rescue moved to it sleeps there carries the sleep with
/// the task to worker 0 and retires, so worker 0 is lazy again (#3117): the next round's rescue
/// turns the locks on a second time. Before, the spare declined while the sleep was pending
/// (sched_relax.velt `keeps`), and worker 0 stayed shared (every run on macOS).
#[test]
fn a_task_sleeping_on_a_retiring_spare_goes_to_worker_0_with_its_sleep() {
    if !has_threads() {
        return;
    }
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "sleeper_on_spare", SLEEPER_ON_SPARE);
    for i in 0..RUNS {
        let r = run(&exe, "1");
        let what = format!("run {i}");
        check(&r, &what, "slept 0\nround 0\nslept 1\nround 1\n", 0);
        assert!(
            rescue_count(&r.err, "locks-on").is_some_and(|n| n >= 2),
            "{what}: worker 0 was not lazy again after the first rescue: {}",
            r.err
        );
    }
}

/// The program of [`a_task_sleeping_on_a_retiring_spare_goes_to_worker_0_with_its_sleep`].
const SLEEPER_ON_SPARE: &str = r#"const MOVED = new Atomic<i64>(0);

// Moved to a spare by the rescue of `main`'s spin, it sleeps there: the spare is asked to retire
// while that sleep is pending.
async function sleeper(r: i64): Promise<void> {
  MOVED.store(r + 1);
  await sleep(300);
  console.log(`slept ${r}`);
}

async function main() {
  for (let r = 0; r < 2; r++) {
    const t = spawn(sleeper(r));
    // Only a rescue runs the task while this poll spins.
    while (MOVED.load() <= r) {}
    await t;
    console.log(`round ${r}`);
  }
}
"#;
