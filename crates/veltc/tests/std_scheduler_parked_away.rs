//! A promise of a task handed to another task and dropped or freed there, on another worker,
//! while the task that started it is parked for ever (#3323, std/lang/hooks/sched_node_lost.velt
//! `droppedAway`): the drop gives its reference to the task and wakes it, and the task's own poll
//! gives the promise up (or frees it, finished) and then finds itself parked for ever, as #3274
//! (`sched_task_lost.velt`) frees a task whose promises it drives itself. Without that, the
//! promise and its task stayed to the end of the program. Each program runs counted with the
//! checking allocator (`VELT_RC_STATS=1`, `VELT_RT_DEBUG_ALLOC=1`) at 4 and 8 workers: exit 0, its
//! output, and every block freed. The programs pin tasks to workers 1 to 3, so not at one worker.

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

/// The issue's program: a task on worker 1 starts a promise that never settles and returns it
/// (in an object) to `main`, which hands it to a task on worker 2 that drops it unawaited. The
/// drop used to give back only the owner's reference, on worker 2, and the promise and the task
/// on worker 1 stayed: 9 blocks left.
#[test]
fn a_suspended_promise_dropped_on_another_worker_frees_its_task() {
    check("handed_drop", HANDED_DROP, "dropped on worker 2: 1\n", 3);
}

/// A promise handed on finishes and is awaited and freed on the task it was handed to, while
/// the task that started it is parked for ever. Its last reference may go there (the owner's
/// drop) or on the worker that woke it (the waker's own reference, released after the wake):
/// either way the promise's reference to its task went on a thread that is not the task's, and
/// nothing polled the task to see that it was now parked for ever (9 blocks a round, 180 in
/// all).
#[test]
fn a_finished_promise_freed_on_another_worker_lets_its_parked_task_go() {
    check(
        "finished_away",
        FINISHED_AWAY,
        "the user got 42 in 20 rounds\n",
        3,
    );
}

/// Two promises of one task dropped at the same moment on two other workers: one hand-over wakes
/// the task while the other still holds its pin on it, so the task may give both up and end in
/// between; the pin keeps it until the second gives it back (5,100 blocks left before, over 300
/// rounds).
#[test]
fn two_promises_dropped_at_once_on_two_workers_free_their_task_once() {
    check(
        "two_droppers",
        TWO_DROPPERS,
        "done 300 rounds, 600 drops\n",
        3,
    );
}

/// A promise that finishes after one turn, dropped on worker 2 at about the moment it finishes,
/// its task then parked for ever: the drop may read the promise's flags before the finish and
/// its count after it (once the task gave its reference back), and must not take a count of 1
/// for a promise its task still holds. Before, 1,500 blocks or more were left every run; with
/// the hand-over decided at the owner's drop only, 1 run in 8 still leaked under load.
#[test]
fn a_promise_dropped_as_it_finishes_lets_its_parked_task_go() {
    check("race_finish", RACE_FINISH, "done 200\n", 3);
}

/// Hand-overs racing the starting task's own polls: 400 tasks each hand a promise (one that never
/// settles, or one that finishes after a few turns) to a task on worker 2 or 3 that drops or
/// awaits it, poll on a while, and then end or park for ever (about 2,200 blocks left before).
#[test]
fn hand_overs_racing_their_tasks_polls_leave_nothing() {
    check("mixed", MIXED, "266 ended, 400 handed on, 100 awaited\n", 3);
}

/// A task whose promise yielded (it has a yield block) ends while that promise, handed on, is
/// still held; the drop of the finished promise on worker 2 gives back the task's last reference
/// there (sched_task_lost.velt `releaseAway`), and that frees the task's yield block with it.
/// Without that, one yield block leaked a round, 20 a run.
#[test]
fn a_task_with_yields_freed_by_a_hand_over_frees_its_yield_block() {
    check("ended_yields", ENDED_YIELDS, "dropped 20 on worker 2\n", 3);
}

/// A holder of a shared promise nobody settles, handed to a task on worker 2 and dropped there
/// after the task it was shared on ended (#3439): the hand-over made a proxy, a node of that task
/// whose waker waits on the promise (sched_share_hand.velt `newProxy`). Its owner gone, the proxy
/// is given up with its role, and the promise and the task go. Before, the proxy waited for ever
/// and kept them: 13 blocks a round, 130 a run.
#[test]
fn a_handed_holders_proxy_dropped_on_another_worker_is_given_up() {
    check("proxy_dropped", PROXY_DROPPED, "done 10\n", 3);
}

/// The program of [`a_handed_holders_proxy_dropped_on_another_worker_is_given_up`].
const PROXY_DROPPED: &str = r#"// #3439, a proxy's owner dropped: the task shares a promise nobody settles (a second holder
// reads it), hands the holder to a task on worker 2 and ends without awaiting it; worker 2 drops
// the holder after the task ended.

const GO = new Atomic<i64>(0);
const DONE = new Atomic<i64>(0);

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function dropLater(h: Holder): Promise<void> {
  while (GO.load() === 0) {
    await yieldNow();
  }
  DONE.add(1);
}

async function maker(): Promise<number> {
  const h = new Holder(dead());
  const q = h.p;
  void spawn(dropLater(h), { worker: 2 });
  return 1;
}

async function main() {
  for (let r = 0; r < 10; r++) {
    GO.store(0);
    await spawn(maker(), { worker: 1 });
    GO.store(1);
    while (DONE.load() <= r) {
      await yieldNow();
    }
  }
  console.log(`done ${DONE.load()}`);
}
"#;

/// The program of [`a_task_with_yields_freed_by_a_hand_over_frees_its_yield_block`].
const ENDED_YIELDS: &str = r#"// #3323 with #3241's yield lists: the task on worker 1 starts a promise that yields a few turns
// and hands it on unawaited (its yields make the task's yield block); the task ends, and worker
// 2 drops the promise, finished, after that: the drop frees the task.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function turns(): Promise<number> {
  await yieldNow();
  await yieldNow();
  return 1;
}

async function maker(): Promise<Holder> {
  const p = turns();
  for (let i = 0; i < 4; i++) {
    await yieldNow();
  }
  return new Holder(p);
}

async function dropper(h: Holder): Promise<number> {
  return 1;
}

async function main() {
  let n = 0;
  for (let i = 0; i < 20; i++) {
    const h = await spawn(maker(), { worker: 1 });
    n += await spawn(dropper(h), { worker: 2 });
  }
  console.log(`dropped ${n} on worker 2`);
}
"#;

/// The program of [`a_suspended_promise_dropped_on_another_worker_frees_its_task`].
const HANDED_DROP: &str = r#"// #3323: a suspended promise handed to a task on another worker and dropped there, while the
// task that started it has ended its root.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

// On worker 1: starts the promise (it runs and suspends under this task), then hands it on.
async function maker(): Promise<Holder> {
  const p = dead();
  await yieldNow();
  return new Holder(p);
}

// On worker 2: drops what it was given, unawaited.
async function dropper(h: Holder): Promise<number> {
  return 1;
}

async function main() {
  const h = await spawn(maker(), { worker: 1 });
  console.log(`dropped on worker 2: ${await spawn(dropper(h), { worker: 2 })}`);
}
"#;

/// The program of [`a_finished_promise_freed_on_another_worker_lets_its_parked_task_go`].
const FINISHED_AWAY: &str = r#"// #3323, the second case: a promise handed to a task on another worker finishes there and is
// freed there, while the task that started it is parked for ever on a promise nobody settles.
// The promise's reference to its task goes on worker 2, and nothing polls the task again.

const DONE = new Atomic<i64>(0);
const GOT = new Atomic<i64>(0);

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function chain(g: Promise<number>): Promise<number> {
  return (await g) + 1;
}

// On worker 2: awaits the chain it was given, and drops it there.
async function user(h: Holder): Promise<void> {
  GOT.store((await h.p) as i64);
  DONE.add(1);
}

// On worker 1: starts a chain on `main`'s promise, hands it to a task on worker 2, and parks for
// ever on a promise nobody settles.
async function maker(open: Promise<number>): Promise<number> {
  void spawn(user(new Holder(chain(open))), { worker: 2 });
  return await dead();
}

async function main() {
  for (let r: i64 = 0; r < 20; r++) {
    const gate = Promise.withResolvers<number>();
    void spawn(maker(gate.promise), { worker: 1 });
    await yieldNow();
    gate.resolve(41);
    while (DONE.load() <= r) {
      await yieldNow();
    }
  }
  console.log(`the user got ${GOT.load()} in ${DONE.load()} rounds`);
}
"#;

/// The program of [`two_promises_dropped_at_once_on_two_workers_free_their_task_once`].
const TWO_DROPPERS: &str = r#"// #3323 under contention: a task on worker 1 starts two promises nobody settles, hands one to a
// task on worker 2 and one to a task on worker 3, and ends its root. The two drop theirs at the
// same moment (both spin on one flag, released once both are spinning), so one hands its promise
// to the task while the other is still giving back its own reference: the task may give both up
// and end in between.

const GO = new Atomic<i64>(0);
const READY = new Atomic<i64>(0);
const DONE = new Atomic<i64>(0);

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

// Drops what it was given once round `r` is released.
async function dropper(h: Holder, r: i64): Promise<void> {
  READY.add(1);
  while (GO.load() <= r) {}
  DONE.add(1);
}

async function maker(r: i64): Promise<number> {
  void spawn(dropper(new Holder(dead()), r), { worker: 2 });
  void spawn(dropper(new Holder(dead()), r), { worker: 3 });
  return 1;
}

async function main() {
  let sum = 0;
  for (let r: i64 = 0; r < 300; r++) {
    sum += await spawn(maker(r), { worker: 1 });
    // Both droppers spin before they are released, so they drop at the same moment.
    while (READY.load() < 2 * (r + 1)) {
      await yieldNow();
    }
    GO.store(r + 1);
    while (DONE.load() < 2 * (r + 1)) {
      await yieldNow();
    }
  }
  console.log(`done ${sum} rounds, ${DONE.load()} drops`);
}
"#;

/// The program of [`a_promise_dropped_as_it_finishes_lets_its_parked_task_go`].
const RACE_FINISH: &str = r#"// #3323, a finish racing the drop: a task on worker 1 starts a promise that finishes after one
// turn, hands it to a task on worker 2 that drops it at about the moment it finishes, and parks
// for ever on a promise nobody settles.

const DONE = new Atomic<i64>(0);

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function soon(): Promise<number> {
  await yieldNow();
  return 1;
}

// Drops what it was given after `wait` turns, unawaited.
async function dropper(h: Holder, wait: i64): Promise<void> {
  for (let y: i64 = 0; y < wait; y++) {
    await yieldNow();
  }
  DONE.add(1);
}

async function maker(r: i64): Promise<number> {
  void spawn(dropper(new Holder(soon()), r % 3), { worker: 2 });
  return await dead();
}

async function main() {
  for (let r: i64 = 0; r < 200; r++) {
    void spawn(maker(r), { worker: 1 });
    while (DONE.load() <= r) {
      await yieldNow();
    }
  }
  console.log(`done ${DONE.load()}`);
}
"#;

/// The program of [`hand_overs_racing_their_tasks_polls_leave_nothing`].
const MIXED: &str = r#"// #3323 while the starting task keeps running: 400 tasks on worker 1 each start a promise (one
// that never settles, or one that finishes after a few turns), hand it to a task on worker 2 or 3
// that drops it or awaits it, keep polling a while, and then end or park for ever; the hand-overs
// race the starting task's own polls.

const DONE = new Atomic<i64>(0);
const SUM = new Atomic<i64>(0);

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

// Finishes after `n` turns of its task.
async function soon(n: i64): Promise<number> {
  for (let i: i64 = 0; i < n; i++) {
    await yieldNow();
  }
  return 1;
}

// Drops what it was given after `wait` turns, unawaited.
async function dropper(h: Holder, wait: i64): Promise<void> {
  for (let y: i64 = 0; y < wait; y++) {
    await yieldNow();
  }
  DONE.add(1);
}

// Awaits what it was given (one that finishes) and drops it.
async function user(h: Holder): Promise<void> {
  SUM.add((await h.p) as i64);
  DONE.add(1);
}

async function maker(i: i64): Promise<number> {
  const finishes = i % 2 == 1;
  const h = new Holder(finishes ? soon(i % 6) : dead());
  const w = 2 + (i % 2);
  if (finishes && i % 4 == 1) {
    void spawn(user(h), { worker: w });
  } else {
    void spawn(dropper(h, i % 9), { worker: w });
  }
  for (let y: i64 = 0; y < 4 + (i % 7); y++) {
    await yieldNow();
  }
  if (i % 3 == 0) {
    return await dead();
  }
  return 1;
}

async function main() {
  const waits: Promise<number>[] = [];
  for (let i: i64 = 0; i < 400; i++) {
    if (i % 3 == 0) {
      void spawn(maker(i), { worker: 1 });
    } else {
      waits.push(spawn(maker(i), { worker: 1 }));
    }
  }
  let ended = 0;
  while (waits.length > 0) {
    ended += await waits.pop()!;
  }
  while (DONE.load() < 400) {
    await yieldNow();
  }
  console.log(`${ended} ended, ${DONE.load()} handed on, ${SUM.load()} awaited`);
}
"#;
