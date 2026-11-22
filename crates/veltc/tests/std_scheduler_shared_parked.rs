//! A task whose root awaits a promise shared on that task, which nothing but its holders can
//! reach any more (#3439, std/lang/hooks/sched_share_listed.velt): the promise is listed
//! owner-only on its task, and the hub holds the root's waker without a reference meanwhile, so
//! the task is found parked for ever (sched_task_lost.velt `parks`) and dropped, with the promise
//! and its hub. Before, the hub's clone of the root's waker kept the task, and the task kept the
//! promise, to the end of the program. Each program runs counted with the checking allocator
//! (`VELT_RC_STATS=1`, `VELT_RT_DEBUG_ALLOC=1`, which also checks each such park) at 4 and 8
//! workers: exit 0, its output, and every block freed. The programs pin tasks to workers 1 and 2,
//! so not at one worker.

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
    check_at(name, src, want, runs, &["4", "8"], true);
}

/// [`check`] at the worker counts `workers` (a program that pins no task runs at 1 worker too),
/// every block freed when `freed`.
fn check_at(name: &str, src: &str, want: &str, runs: usize, workers: &[&str], freed: bool) {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), name, src);
    let mut bad = Vec::new();
    for threads in workers {
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
            if o.status.code() != Some(0) || out != want || (freed && !all_freed(&err)) {
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

/// The root awaits a promise nobody settles through a holder shared on its own task (no
/// hand-over): 14 blocks left before.
#[test]
fn a_root_awaiting_a_shared_promise_nobody_settles_is_dropped() {
    check("shared_parked", SHARED_PARKED, "ok\n", 3);
}

/// The issue's program: the root awaits the promise through a holder it handed to a task on
/// worker 2, which drops it later (its proxy is given up first): 12 blocks left before.
#[test]
fn a_root_awaiting_a_shared_holder_handed_on_and_dropped_is_dropped() {
    check("ice_field", ICE_FIELD, "ok\n", 3);
}

/// A holder kept in module state outlives the root's frame: the task is dropped while it is
/// held, and `main` drops it later, off the task (the promise leaves the list there, under the
/// module lock: sched_node_lost.velt `droppedAway`). 120 blocks left before; an `ICE:` without
/// that unlisting.
#[test]
fn a_holder_kept_in_module_state_outlives_the_dropped_task() {
    check("kept_holder", KEPT_HOLDER, "parked 10\n", 3);
}

/// A promise of the task hands a second holder to a task on worker 2 while the root, parked
/// on the promise, is listed with it: the hand-over takes the promise off the list and the
/// hub's entries take their references back: 130 blocks left before.
#[test]
fn a_holder_handed_over_while_the_promise_is_listed_takes_its_references_back() {
    check("handed_listed", HANDED_LISTED, "handed 10\n", 3);
}

/// The promise has a resolver kept in module state, which a task on worker 2 calls: the root
/// is not taken for parked for ever and gets the value.
#[test]
fn a_shared_promise_settled_by_another_task_reaches_its_root() {
    check("settled_away", SETTLED_AWAY, "got 70\n", 3);
}

/// The shared promise yields once before it waits for ever (the #3462 review's program): its
/// holder's wait comes while the yield holds its waker, and the task is found parked for ever
/// once the yield is done. Listing it at that run instead stopped every counted run with an
/// `ICE:`, with its reference still held.
#[test]
fn a_shared_promise_that_yields_before_it_waits_for_ever_is_dropped() {
    check("yield_first", YIELD_FIRST, "ok\n", 3);
}

/// The root races the holder against a few yields first, then awaits it alone, and is found
/// parked for ever: 130 blocks left before.
#[test]
fn a_root_that_raced_a_shared_promise_first_is_dropped() {
    check("raced_first", RACED_FIRST, "ok\n", 3);
}

/// The program of [`a_root_awaiting_a_shared_promise_nobody_settles_is_dropped`].
const SHARED_PARKED: &str = r#"// #3439 variant: no hand-over. The task awaits a promise nobody settles through a holder that is
// shared on its own task (kept in an array as well), and parks for ever.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function maker(): Promise<number> {
  const h = new Holder(dead());
  const keep: Holder[] = [h];
  const q = keep[0].p;
  return await h.p;
}

async function main() {
  void spawn(maker(), { worker: 1 });
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  console.log("ok");
}
"#;

/// The program of [`a_root_awaiting_a_shared_holder_handed_on_and_dropped_is_dropped`].
const ICE_FIELD: &str = r#"// Probe: the starting task awaits the promise through a shared holder (its adoption poll lists it
// owner-only), parks for ever and is cancelled; the holder's last reference is then dropped on
// worker 2, so the listed promise's handle goes off its task.

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
  void spawn(dropLater(h), { worker: 2 });
  return await h.p;
}

async function main() {
  void spawn(maker(), { worker: 1 });
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  GO.store(1);
  while (DONE.load() === 0) {
    await yieldNow();
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  console.log("ok");
}
"#;

/// The program of [`a_holder_kept_in_module_state_outlives_the_dropped_task`].
const KEPT_HOLDER: &str = r#"// #3439: a holder of the shared promise kept in module state outlives the root's frame. The task
// awaits the promise (nobody settles it), is found parked for ever and cancelled; main drops the
// holders later.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const KEPT: Holder[] = [];
const PARKED = new Atomic<i64>(0);

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function maker(): Promise<number> {
  const h = new Holder(dead());
  KEPT.push(h);
  PARKED.add(1);
  return await h.p;
}

async function main() {
  for (let r = 0; r < 10; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  while (PARKED.load() < 10) {
    await yieldNow();
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  KEPT.length = 0;
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  console.log(`parked ${PARKED.load()}`);
}
"#;

/// The program of [`a_holder_handed_over_while_the_promise_is_listed_takes_its_references_back`].
const HANDED_LISTED: &str = r#"// #3439: a holder handed to another task while its promise is listed. The task's root awaits a
// shared promise nobody settles (listed, the root parked); a promise of the same task runs on a
// few turns later and hands a second holder to a task on worker 2, which drops it. The hand-over
// takes the promise off the list; once the handed holder is gone, the task is parked for ever and
// cancelled.

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

async function user(h: Holder): Promise<void> {
  await yieldNow();
  DONE.add(1);
}

async function later(h: Holder): Promise<number> {
  await yieldNow();
  await yieldNow();
  void spawn(user(h), { worker: 2 });
  return 1;
}

async function maker(): Promise<number> {
  const h = new Holder(dead());
  const g = new Holder(h.p);
  void later(g);
  return await h.p;
}

async function main() {
  for (let r = 0; r < 10; r++) {
    void spawn(maker(), { worker: 1 });
  }
  while (DONE.load() < 10) {
    await yieldNow();
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  console.log(`handed ${DONE.load()}`);
}
"#;

/// The program of [`a_shared_promise_settled_by_another_task_reaches_its_root`].
const SETTLED_AWAY: &str = r#"// #3439: a shared promise the root awaits, handed to a task on worker 2 that settles it (through
// a resolver kept in module state): the root must not be taken for parked for ever; it gets the
// value.

const GOT = new Atomic<i64>(0);
let RESOLVE: ((v: number) => void)[] = [];

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function later(): Promise<number> {
  return await new Promise<number>((resolve) => {
    RESOLVE.push(resolve);
  });
}

async function settler(h: Holder): Promise<void> {
  await yieldNow();
  const r = RESOLVE.pop()!;
  r(7);
}

async function maker(): Promise<number> {
  const h = new Holder(later());
  void spawn(settler(new Holder(h.p)), { worker: 2 });
  const v = await h.p;
  GOT.add(v as i64);
  return v;
}

async function main() {
  for (let r = 0; r < 10; r++) {
    await spawn(maker(), { worker: 1 });
  }
  console.log(`got ${GOT.load()}`);
}
"#;

/// The program of [`a_shared_promise_that_yields_before_it_waits_for_ever_is_dropped`].
const YIELD_FIRST: &str = r#"// #3462 review: a shared promise that yields once before it waits for ever, awaited by its
// task's root through a holder shared on the task: found parked for ever, with nothing left.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function dead(): Promise<number> {
  await yieldNow();
  return await new Promise<number>((resolve) => {});
}

async function maker(): Promise<number> {
  const h = new Holder(dead());
  const q = h.p;
  return await q;
}

async function main() {
  for (let r = 0; r < 10; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  console.log("ok");
}
"#;

/// The program of [`a_root_that_raced_a_shared_promise_first_is_dropped`].
const RACED_FIRST: &str = r#"// #3439: a holder's waiting entry changes its waker while the promise is listed: the root races
// the holder against a few yields first (the race's waker), then awaits it alone (the root's),
// and is found parked for ever.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function turns(): Promise<number> {
  await yieldNow();
  await yieldNow();
  return 0;
}

async function maker(): Promise<number> {
  const h = new Holder(dead());
  const q = h.p;
  await Promise.race([q, turns()]);
  return await q;
}

async function main() {
  for (let r = 0; r < 10; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  console.log("ok");
}
"#;

/// A park's verdict going stale before the cancel poll (the #3462 review's `p_kept_handed`):
/// tasks keep a holder in module state and park on it, and `main` hands each over to worker 3 as
/// soon as its task parked, which can take the promise off the task's list in between. The cancel
/// poll looks again and polls the task as any other (sched_task_lost.velt `stillLost`); without that
/// second look, `lt.cancel`'s counted check stops it with an `ICE:` (59 runs of 60). The holder
/// handed over is dropped by `main`, under the module lock: that wakes its promise's task, whose
/// next poll lists the promise again (sched_share_listed.velt `relist`, #3476); without it, about
/// half the runs left 1 to 18 of the 30 tasks.
#[test]
fn a_park_whose_verdict_went_stale_does_not_drop_its_task() {
    check("kept_handed", KEPT_HANDED, "handed 30, used 30\n", 3);
}

/// The program of [`a_park_whose_verdict_went_stale_does_not_drop_its_task`].
const KEPT_HANDED: &str = r#"// #3462 review: a park's verdict going stale before the cancel poll. Tasks keep a holder of a
// promise nobody settles in module state and park on it; main hands each kept holder to a task on
// worker 3 as soon as its task parked, so the hand-over (which takes the promise off its task's
// list) can land between that task's park and its last poll. The task must not be dropped then:
// its cancel poll looks again.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const KEPT: Holder[] = [];
const PARKED = new Atomic<i64>(0);
const USED = new Atomic<i64>(0);

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function maker(): Promise<number> {
  const h = new Holder(dead());
  KEPT.push(h);
  PARKED.add(1);
  return await h.p;
}

async function user(h: Holder): Promise<void> {
  await yieldNow();
  USED.add(1);
}

async function main() {
  const n: i64 = 30;
  for (let r: i64 = 0; r < n; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  let handed: i64 = 0;
  while (handed < n) {
    while (KEPT.length > 0) {
      void spawn(user(KEPT.pop()!), { worker: 3 });
      handed += 1;
    }
    await yieldNow();
  }
  while (USED.load() < n) {
    await yieldNow();
  }
  console.log(`handed ${handed}, used ${USED.load()}`);
}
"#;

/// Two holders of one promise nobody settles, kept in module state: `main` drops one copy, off
/// the promise's task and under the module lock (the drop wakes the task, sched_share_listed.velt
/// `relist`), then hands the other to worker 3, which drops it there. Without the wake about 80
/// runs of 100 left blocks (#3476).
#[test]
fn two_kept_copies_dropped_and_handed_over_leave_nothing() {
    check("two_copies", TWO_COPIES, "handed 30, used 30\n", 3);
}

/// The program of [`two_kept_copies_dropped_and_handed_over_leave_nothing`].
const TWO_COPIES: &str = r#"// #3476: two holders of one promise nobody settles, both kept in module state; main drops one
// copy (off the promise's task, under the module lock: the drop wakes the task), then hands the
// other to a task on worker 3, which drops it there. Every task is found parked for ever.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const KEPT: Holder[] = [];
const COPIES: Holder[] = [];
const USED = new Atomic<i64>(0);

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function maker(): Promise<number> {
  const h = new Holder(dead());
  COPIES.push(new Holder(h.p));
  KEPT.push(h);
  return await h.p;
}

async function user(h: Holder): Promise<void> {
  await yieldNow();
  USED.add(1);
}

async function main() {
  const n: i64 = 30;
  for (let r: i64 = 0; r < n; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  let handed: i64 = 0;
  while (handed < n) {
    while (KEPT.length > 0 && COPIES.length > 0) {
      COPIES.pop();
      void spawn(user(KEPT.pop()!), { worker: 3 });
      handed += 1;
    }
    await yieldNow();
  }
  while (USED.load() < n) {
    await yieldNow();
  }
  console.log(`handed ${handed}, used ${USED.load()}`);
}
"#;
