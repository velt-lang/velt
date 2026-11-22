//! A promise of the task (not its root) waiting in the hub of a shared promise that is listed
//! owner-only (#3487, std/lang/hooks/sched_share_listed.velt `waiting`): the hub holds its waker
//! without a reference too and it is listed, so its task is found parked for ever; it leaves the
//! list when the shared promise does, or when its owner hands it over. Each program runs counted
//! with the checking allocator (`VELT_RC_STATS=1`, `VELT_RT_DEBUG_ALLOC=1`) at 4 and 8 workers:
//! exit 0, its output, and every block freed unless the test says otherwise. The programs pin
//! tasks to workers 1 to 3, so not at one worker.

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

/// A promise of the task (not its root) waits in the hub of a shared promise nobody settles, and
/// the root waits there too: while the shared promise is listed, the hub holds the waiting
/// promise's waker without a reference and it is listed as well (sched_share_listed.velt
/// `waiting`, `sweep`), so the task is found parked for ever. Before, every run left 280 blocks
/// (#3487).
#[test]
fn a_promise_of_the_task_waiting_in_a_listed_hub_is_listed_too() {
    check("waiting_in_hub", WAITING_IN_HUB, "parked 20\n", 3);
}

/// As [`a_promise_of_the_task_waiting_in_a_listed_hub_is_listed_too`], with a promise of the task
/// that hands a second holder to worker 3 while both are listed: the hand-over takes the shared
/// promise off the list, and with it the waiting promise, whose entry takes its reference back
/// (sched_share_listed.velt `unlisted`). Before, every run left 300 blocks; giving the reference
/// back but leaving the promise listed stopped every run with an `ICE:`, and the reverse left 300
/// blocks.
#[test]
fn a_waiting_promise_unlisted_by_a_hand_over_takes_its_reference_back() {
    check("waiting_handed", WAITING_HANDED, "used 20\n", 3);
}

/// A chain of two: a third promise of the task awaits the promise waiting in the listed hub.
/// Before, every run left 300 blocks.
#[test]
fn a_promise_awaiting_a_promise_waiting_in_a_listed_hub_is_dropped_too() {
    check("waiting_chain", WAITING_CHAIN, "parked 20\n", 3);
}

/// A promise of the task listed while it waits in the hub (its owner still running) is handed to
/// a task on worker 3 by its owner, which drops it there: it leaves the list as it is handed over
/// (sched_share_listed.velt `leaveWaiting`), and the drop does not stop the run. Without that,
/// every run stopped with an `ICE:` (a listed promise dropped off its task). The task is not
/// freed: a promise of the task handed over while it waits in the task's hub keeps it, as before
/// this listing (#3487), so the blocks are not checked.
#[test]
fn a_listed_waiting_promise_handed_over_by_its_owner_leaves_the_list() {
    check_at(
        "waiting_moved",
        WAITING_MOVED,
        "used 20\n",
        3,
        &["4", "8"],
        false,
    );
}

/// The owner shares a promise of the task listed while it waits in the hub: it stays listed, now
/// as a shared promise whose hub holds the root's waker without a reference, and the task is found
/// parked for ever.
#[test]
fn a_listed_waiting_promise_shared_by_its_owner_stays_listed() {
    check("waiting_shared", WAITING_SHARED, "parked 20\n", 3);
}

/// After the share, the owner hands a holder of the shared waiting promise to worker 3, then a
/// holder of the first shared promise: the waiting promise leaves the list through its own hub and
/// is listed again as a shared promise; unlisting the first promise counts its entry again and
/// takes it off the list (sched_share_listed.velt `unlisted`). Without that, it stayed listed with
/// a counted waker, and every counted run stopped with an `ICE:`. The task is not freed (as before this listing), so the
/// blocks are not checked.
#[test]
fn a_shared_listed_waiting_promise_handed_over_gives_its_first_entry_back() {
    check_at(
        "waiting_shared_twice",
        WAITING_SHARED_TWICE,
        "parked 20\n",
        3,
        &["4", "8"],
        false,
    );
}

/// The owner races a promise of the task listed while it waits in the hub against one that
/// finishes first, then hands it to worker 3: the race shares it, so it leaves the list through
/// its own hub's hand-over (sched_share_listed.velt `leave`), and every task is still found parked
/// for ever. Before, every run left 340 blocks.
#[test]
fn a_listed_waiting_promise_raced_then_handed_over_is_dropped() {
    check("race_handed", RACE_HANDED, "used 20\n", 3);
}

/// The program of [`a_listed_waiting_promise_raced_then_handed_over_is_dropped`].
const RACE_HANDED: &str = r#"// #3502 review: a promise of the task (`side`) is listed while it waits in a shared promise's hub;
// then its owner races it against a promise that finishes first (the race's poll waits in its
// place), and hands it to a task on worker 3, which drops it there.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const KEPT: Holder[] = [];
const USED = new Atomic<i64>(0);

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function inner(h: Holder): Promise<number> {
  return await h.p;
}

async function soon(): Promise<number> {
  await yieldNow();
  return 2;
}

async function user(p: Promise<number>): Promise<void> {
  await yieldNow();
  USED.add(1);
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const side = inner(d);
  const r = inner(d);
  await yieldNow();
  await yieldNow();
  const won = await Promise.race([side, soon()]);
  void spawn(user(side), { worker: 3 });
  return won + (await d.p) + (await r);
}

async function main() {
  const n: i64 = 20;
  for (let i: i64 = 0; i < n; i++) {
    void spawn(maker(), { worker: 1 + (i % 2) });
  }
  while (USED.load() < n) {
    await yieldNow();
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  KEPT.length = 0;
  console.log(`used ${USED.load()}`);
}
"#;

/// The program of [`a_shared_listed_waiting_promise_handed_over_gives_its_first_entry_back`].
const WAITING_SHARED_TWICE: &str = r#"// #3487 second review: as ns_waiting_shared, then the owner hands a holder of `side` (shared by
// then, and listed) to worker 3, and later a holder of the first shared promise: `side` leaves
// the list through its own hub, is listed again as a shared promise, and leaves it again as the
// first promise leaves it.

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

async function inner(h: Holder): Promise<number> {
  return await h.p;
}

async function user(h: Holder): Promise<void> {
  await yieldNow();
  await yieldNow();
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const side = inner(d);
  const r = inner(d);
  await yieldNow();
  await yieldNow();
  const s = new Holder(side);
  KEPT.push(s);
  const q = s.p;
  await yieldNow();
  void spawn(user(new Holder(q)), { worker: 3 });
  await yieldNow();
  await yieldNow();
  await yieldNow();
  void spawn(user(new Holder(d.p)), { worker: 3 });
  PARKED.add(1);
  return (await d.p) + (await r) + (await q);
}

async function main() {
  const n: i64 = 20;
  for (let i: i64 = 0; i < n; i++) {
    void spawn(maker(), { worker: 1 + (i % 2) });
  }
  while (PARKED.load() < n) {
    await yieldNow();
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  KEPT.length = 0;
  console.log(`parked ${PARKED.load()}`);
}
"#;

/// The program of [`a_listed_waiting_promise_handed_over_by_its_owner_leaves_the_list`].
const WAITING_MOVED: &str = r#"// #3487 review: a promise of the task (`side`) is listed while it waits in a shared promise's hub,
// its owner still running: a second promise of the task waits there too, which lists the shared
// promise and `side` with it. Then the owner hands `side` to a task on worker 3, which drops it
// there, and waits for ever itself. `side` leaves the list as it is handed over.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const KEPT: Holder[] = [];
const USED = new Atomic<i64>(0);

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function inner(h: Holder): Promise<number> {
  return await h.p;
}

async function user(p: Promise<number>): Promise<void> {
  await yieldNow();
  USED.add(1);
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const side = inner(d);
  const r = inner(d);
  await yieldNow();
  await yieldNow();
  void spawn(user(side), { worker: 3 });
  return (await d.p) + (await r);
}

async function main() {
  const n: i64 = 20;
  for (let i: i64 = 0; i < n; i++) {
    void spawn(maker(), { worker: 1 + (i % 2) });
  }
  while (USED.load() < n) {
    await yieldNow();
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  KEPT.length = 0;
  console.log(`used ${USED.load()}`);
}
"#;

/// The program of [`a_listed_waiting_promise_shared_by_its_owner_stays_listed`].
const WAITING_SHARED: &str = r#"// #3487 review: as ns_waiting_moved, but the owner shares `side` once it is listed: a holder of
// it goes in module state, which main drops at the end, and the owner reads it (the read shares
// it). It stays listed, now as a shared promise, and the task is found parked for ever.

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

async function inner(h: Holder): Promise<number> {
  return await h.p;
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const side = inner(d);
  const r = inner(d);
  await yieldNow();
  await yieldNow();
  const s = new Holder(side);
  KEPT.push(s);
  const q = s.p;
  PARKED.add(1);
  return (await d.p) + (await r) + (await q);
}

async function main() {
  const n: i64 = 20;
  for (let i: i64 = 0; i < n; i++) {
    void spawn(maker(), { worker: 1 + (i % 2) });
  }
  while (PARKED.load() < n) {
    await yieldNow();
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  KEPT.length = 0;
  console.log(`parked ${PARKED.load()}`);
}
"#;

/// The program of [`a_promise_of_the_task_waiting_in_a_listed_hub_is_listed_too`].
const WAITING_IN_HUB: &str = r#"// #3487: a promise of the task (`side`, not the root) waits in the hub of a shared promise nobody
// settles, and the root waits there too, after a yield; a holder is kept in module state, which
// main drops at the end. Every task is found parked for ever.

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

async function inner(h: Holder): Promise<number> {
  return await h.p;
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const side = inner(d);
  await yieldNow();
  PARKED.add(1);
  return (await d.p) + (await side);
}

async function main() {
  const n: i64 = 20;
  for (let r: i64 = 0; r < n; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  while (PARKED.load() < n) {
    await yieldNow();
  }
  for (let i = 0; i < 50; i++) {
    await yieldNow();
  }
  KEPT.length = 0;
  console.log(`parked ${PARKED.load()}`);
}
"#;

/// The program of [`a_waiting_promise_unlisted_by_a_hand_over_takes_its_reference_back`].
const WAITING_HANDED: &str = r#"// #3487: as ns_waiting_in_hub, but a third promise of the task hands a second holder to a task
// on worker 3 a few turns after the root and `side` wait in the hub, on the task's own thread:
// that takes the shared promise off the list, and `side` with it, until the holder is dropped
// there. Every task is still found parked for ever.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const USED = new Atomic<i64>(0);

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function inner(h: Holder): Promise<number> {
  return await h.p;
}

async function user(h: Holder): Promise<void> {
  await yieldNow();
  USED.add(1);
}

async function later(h: Holder): Promise<number> {
  await yieldNow();
  await yieldNow();
  void spawn(user(h), { worker: 3 });
  return 1;
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  const side = inner(d);
  void later(new Holder(d.p));
  return (await d.p) + (await side);
}

async function main() {
  const n: i64 = 20;
  for (let r: i64 = 0; r < n; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  while (USED.load() < n) {
    await yieldNow();
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  console.log(`used ${USED.load()}`);
}
"#;

/// The program of [`a_promise_awaiting_a_promise_waiting_in_a_listed_hub_is_dropped_too`].
const WAITING_CHAIN: &str = r#"// #3487: as ns_waiting_in_hub, with a chain of two: a third promise of the task, `q`, awaits
// `side`, which waits in the hub of the shared promise nobody settles. Every task is still
// found parked for ever.

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

async function inner(h: Holder): Promise<number> {
  return await h.p;
}

async function outer(p: Promise<number>): Promise<number> {
  return await p;
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const side = inner(d);
  const q = outer(side);
  await yieldNow();
  PARKED.add(1);
  return (await d.p) + (await q);
}

async function main() {
  const n: i64 = 20;
  for (let r: i64 = 0; r < n; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  while (PARKED.load() < n) {
    await yieldNow();
  }
  for (let i = 0; i < 50; i++) {
    await yieldNow();
  }
  KEPT.length = 0;
  console.log(`parked ${PARKED.load()}`);
}
"#;
