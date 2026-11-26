//! A task parked for ever while its frames hold a promise that already finished (#3498,
//! std/lang/hooks/sched_node_done.velt): such a promise can never wake the task, but it kept its
//! reference to the task until it was freed, so the park's count had one reference too many and
//! the task, with what it holds, stayed to the end of the program. A finished promise now gives
//! that reference back as it finishes, when nothing but its owner (or its hub) can reach it. Each
//! program runs counted with the checking allocator (`VELT_RC_STATS=1`, `VELT_RT_DEBUG_ALLOC=1`)
//! at 4 and 8 workers: exit 0, its output, and every block freed.

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
    check_at(name, src, want, runs, &["4", "8"], true, &[]);
}

/// [`check`] with `VELT_SCHED_TASK_PAUSE` (std/lang/hooks/sched_wake_pause.velt): a reader off
/// a node's task gives its core away between reading the node unfinished and reading its task,
/// so the node finishes there, and lets go of its task, in between.
fn check_paused(name: &str, src: &str, want: &str, runs: usize) {
    check_at(
        name,
        src,
        want,
        runs,
        &["4", "8"],
        true,
        &[("VELT_SCHED_TASK_PAUSE", "2000")],
    );
}

/// Runs `exe` at `threads` workers, counted, with `env`, killed after a minute (a lost wake
/// waits for ever): its exit code (`None` when killed), stdout and stderr.
fn run(exe: &Path, threads: &str, env: &[(&str, &str)]) -> (Option<i32>, String, String) {
    run_command(command(exe), threads, env)
}

/// [`run`] of a command that runs the program (the program itself, or a wrapper that becomes it).
fn run_command(
    mut cmd: std::process::Command,
    threads: &str,
    env: &[(&str, &str)],
) -> (Option<i32>, String, String) {
    let mut child = cmd
        .env("VELT_THREADS", threads)
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env("VELT_RC_STATS", "1")
        .env_remove("VELT_RESCUE")
        .env_remove("VELT_SCHEDULER")
        .envs(env.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        // timing: polls the child for its exit under a one-minute cap, assumes no timing
        std::thread::sleep(Duration::from_millis(20));
    };
    let o = child.wait_with_output().unwrap();
    (
        code,
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// [`check`] at the worker counts `workers` (a program that pins no task runs at 1 worker too),
/// every block freed when `freed`, with `env` set.
fn check_at(
    name: &str,
    src: &str,
    want: &str,
    runs: usize,
    workers: &[&str],
    freed: bool,
    env: &[(&str, &str)],
) {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), name, src);
    let mut bad = Vec::new();
    for threads in workers {
        for i in 0..runs {
            let (code, out, err) = run(&exe, threads, env);
            if code != Some(0) || out != want || (freed && !all_freed(&err)) {
                bad.push(format!(
                    "{threads} workers, run {i}: exit {code:?}, stdout {out:?}, stderr {err}"
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

/// The root waits for ever while its frame holds a promise that finished without being
/// awaited (sched_node_done.velt `letGo`). Before, every run left 280 blocks.
#[test]
fn a_task_holding_a_promise_that_finished_unawaited_is_found_parked_for_ever() {
    check("held_plain", HELD_PLAIN, "parked 20\n", 3);
}

/// The same with promises that can reject but fulfilled, one held unawaited and one awaited
/// (#3595): they let go of their task too (sched_node_done.velt `settledWell`). Before, only a
/// promise that cannot reject did, and every run left blocks.
#[test]
fn a_task_holding_a_fulfilled_promise_that_could_reject_is_found_parked_for_ever() {
    check("held_can_reject", HELD_CAN_REJECT, "parked 20\n", 3);
}

/// #3487's `two_roots`: the frame keeps a holder of a shared promise that settled (its hub
/// keeps it, and it lets go of its task as it settles), and a promise of the task waits in
/// a listed hub. Before, every run left 460 blocks.
#[test]
fn a_task_holding_a_finished_shared_promise_is_found_parked_for_ever() {
    check("two_roots", TWO_ROOTS, "parked 20\n", 3);
}

/// The finished promise is handed to a task on worker 3 before the root parks, which reads
/// and drops it there: the task is found parked for ever, and nothing is given back twice.
#[test]
fn a_finished_promise_handed_to_another_task_is_not_counted() {
    check("held_handed", HELD_HANDED, "parked 20, used 20\n", 3);
}

/// The finished promise is also kept in module state, which `main` drops off its task at
/// the end: its free there gives nothing back to the task, which it let go of as it
/// finished.
#[test]
fn a_finished_promise_kept_in_module_state_is_freed_off_its_task() {
    check("held_kept", HELD_KEPT, "parked 20\n", 3);
}

/// A promise that finished, and let go of its task, is shared after: its owner reads it
/// through a holder and hands another holder to worker 3, which reads it too. Its hub takes
/// it for another task's and settles at the first holder's poll. Before, every run left
/// 340 blocks. It guards the let-go itself; the hub's choice of `AWAY` is guarded by the
/// one below.
#[test]
fn a_finished_promise_shared_after_it_let_go_of_its_task_is_read() {
    check("share_finished", SHARE_FINISHED, "parked 40, read 40\n", 3);
}

/// A promise that finished, and let go of its task, is shared after on that task, and its
/// original holder is handed to worker 3, which reads it after the sharing task read and
/// dropped its copy. The hub must take the node for another task's (`AWAY`): taken as
/// `HUB_HERE`, the original's drop (sched_share.velt `holderDrop`, the `ORIG_GONE` path)
/// would leave the free to a finish still to come, and for a finished node none comes.
/// Before, every run left 80 blocks; with `HUB_HERE` for such a node, 40.
#[test]
fn a_finished_promise_shared_after_it_let_go_with_its_original_handed_over_is_freed() {
    check("share_orig_handed", SHARE_ORIG_HANDED, "read 80\n", 3);
}

/// As the one above, with the holders kept in module state and dropped one at a time by
/// `main`, off the task and under the module lock (sched_share_listed.velt `relist` looks at
/// each drop). Before, every run left 320 blocks.
#[test]
fn a_finished_promise_shared_into_module_state_is_dropped_off_its_task() {
    check("share_kept", SHARE_KEPT, "parked 20\n", 3);
}

/// Promises handed to another worker while still pending: one is awaited there through a
/// `Promise.all` join (whose waker maps to no task), the next dropped unawaited, each finishing,
/// and letting go of its task, inside the paused window between the owner's load of its flags
/// and of its task (sched_node_await.velt `awaitWith` and `ownSide`: task 0 is no task to them).
/// With task 0 taken for the join's own, the await stored its waker with no second look at the
/// flags, and nothing woke it: every run ended with nothing printed and blocks left. With task 0
/// taken for the dropper's own side, its stale flags left the result undropped: 30 blocks left
/// in every run.
#[test]
fn promises_handed_over_pending_and_awaited_or_dropped_there_as_they_finish() {
    check_paused("handed_all", HANDED_ALL, "got 30, dropped 30\n", 3);
}

/// A promise handed to worker 2 and shared there (its hub is `AWAY`): the first holder to
/// wait leaves in a race as the promise finishes and lets go of its task, and the hub then
/// awaits it for the next waiter, a `Promise.all` join (sched_share_away.velt `watch`,
/// sched_node_await.velt `awaitWith`). With task 0 taken for the join's own, that waiter was
/// never woken: every run ended with nothing printed and blocks left.
#[test]
fn a_shared_promise_whose_first_waiter_leaves_after_it_finished_still_wakes_the_next() {
    check_paused("away_first_leaves", AWAY_FIRST_LEAVES, "got 30\n", 3);
}

/// #3845: with fewer cores than workers each turn a pause gives away (`VELT_SCHED_TASK_PAUSE`,
/// sched_wake_pause.velt) hands the core to another thread for far longer than on an idle machine,
/// and this program pauses 240 times: 2000 turns each took it past the minute on a loaded 3-core
/// machine. A pause now ends by the clock too. Here: one core for 8 workers and five times the
/// turns, which is minutes without that bound and seconds with it. Linux only: `taskset` pins it.
#[test]
#[cfg(target_os = "linux")]
fn a_paused_program_ends_in_time_with_one_core_for_its_workers() {
    let Some(cpu) = first_allowed_cpu() else {
        return;
    };
    if command("taskset").arg("--version").output().is_err() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "away_one_core", AWAY_FIRST_LEAVES);
    let mut pinned = command("taskset");
    pinned.arg("-c").arg(&cpu).arg(&exe);
    let (code, out, err) = run_command(pinned, "8", &[("VELT_SCHED_TASK_PAUSE", "10000")]);
    assert!(
        code == Some(0) && out == "got 30\n" && all_freed(&err),
        "pinned to cpu {cpu}: exit {code:?}, stdout {out:?}, stderr {err}"
    );
}

/// The first CPU this process may run on (`Cpus_allowed_list` of /proc/self/status).
#[cfg(target_os = "linux")]
fn first_allowed_cpu() -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let list = status
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))?;
    let first = list.trim().split([',', '-']).next()?;
    (!first.is_empty()).then(|| first.to_string())
}

/// The program of [`a_task_holding_a_promise_that_finished_unawaited_is_found_parked_for_ever`].
const HELD_PLAIN: &str = r#"// #3498: the root waits for ever on a shared promise nobody settles while its frame holds a
// promise (`f`, not shared) that finished without being awaited: the finished one can never wake
// the task, so every task is still found parked for ever.

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

async function soon(): Promise<number> {
  await yieldNow();
  return 1;
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const f = soon();
  for (let i = 0; i < 5; i++) {
    await yieldNow();
  }
  PARKED.add(1);
  return (await d.p) + (await f);
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

/// The program of [`a_task_holding_a_fulfilled_promise_that_could_reject_is_found_parked_for_ever`].
const HELD_CAN_REJECT: &str = r#"// #3595: `held_plain` with promises that can reject: the root waits for ever on a shared promise
// nobody settles while its frame holds one that fulfilled (`f`) and another that fulfilled and was
// awaited (`g`). Neither can wake the task or be reported, so every task is still found parked
// for ever.

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

async function soon(fail: boolean): Promise<number> {
  await yieldNow();
  if (fail) {
    throw new Error("never");
  }
  return 1;
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const f = soon(false);
  const g = soon(false);
  const v = await g;
  for (let i = 0; i < 5; i++) {
    await yieldNow();
  }
  PARKED.add(1);
  return (await d.p) + (await f) + v;
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

/// The program of [`a_task_holding_a_finished_shared_promise_is_found_parked_for_ever`].
const TWO_ROOTS: &str = r#"// #3462 review's p_two_roots shape: the root awaits two holders in turn (the first settles), and a
// promise of the task awaits a third holder of a dead promise kept in module state.
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

async function soon(): Promise<number> {
  await yieldNow();
  return 1;
}

async function inner(h: Holder): Promise<number> {
  return await h.p;
}

async function maker(): Promise<number> {
  const a = new Holder(soon());
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const side = inner(d);
  const x = await a.p;
  PARKED.add(1);
  return x + (await d.p) + (await side);
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

/// The program of [`a_finished_promise_handed_to_another_task_is_not_counted`].
const HELD_HANDED: &str = r#"// #3498: as held_plain, but the finished promise is handed to a task on worker 3 before the root
// parks: a promise another task holds is not counted as held by this one, so the park doesn't say
// yes early, and the task is found parked for ever once worker 3 has dropped it.

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

async function soon(): Promise<number> {
  await yieldNow();
  return 1;
}

async function user(p: Promise<number>): Promise<void> {
  for (let i = 0; i < 20; i++) {
    await yieldNow();
  }
  USED.add((await p) as i64);
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const f = soon();
  for (let i = 0; i < 5; i++) {
    await yieldNow();
  }
  void spawn(user(f), { worker: 3 });
  PARKED.add(1);
  return await d.p;
}

async function main() {
  const n: i64 = 20;
  for (let r: i64 = 0; r < n; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  while (PARKED.load() < n || USED.load() < n) {
    await yieldNow();
  }
  for (let i = 0; i < 50; i++) {
    await yieldNow();
  }
  KEPT.length = 0;
  console.log(`parked ${PARKED.load()}, used ${USED.load()}`);
}
"#;

/// The program of [`a_finished_promise_kept_in_module_state_is_freed_off_its_task`].
const HELD_KEPT: &str = r#"// #3498: as held_plain, but the finished promise is also kept in module state, which `main` drops
// at the end, off its task: it leaves the task's count as it is freed there, and every task is
// still found parked for ever.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const KEPT: Holder[] = [];
const DONE: Promise<number>[] = [];
const PARKED = new Atomic<i64>(0);

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function soon(): Promise<number> {
  await yieldNow();
  return 1;
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const f = soon();
  for (let i = 0; i < 5; i++) {
    await yieldNow();
  }
  DONE.push(f);
  PARKED.add(1);
  return await d.p;
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
  for (let i = 0; i < 50; i++) {
    await yieldNow();
  }
  DONE.length = 0;
  console.log(`parked ${PARKED.load()}`);
}
"#;

/// The program of [`a_finished_promise_shared_after_it_let_go_of_its_task_is_read`].
const SHARE_FINISHED: &str = r#"// #3498: a promise that finished, and so let go of its task, is shared after: its owner reads it
// through a holder, and hands a second holder to a task on worker 3, which reads it there. Then
// the task parks for ever: every value arrives and every task is found parked for ever.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const KEPT: Holder[] = [];
const PARKED = new Atomic<i64>(0);
const READ = new Atomic<i64>(0);

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function soon(): Promise<number> {
  await yieldNow();
  return 2;
}

async function reader(h: Holder): Promise<void> {
  await yieldNow();
  READ.add((await h.p) as i64);
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const f = soon();
  for (let i = 0; i < 5; i++) {
    await yieldNow();
  }
  const h = new Holder(f);
  const v = await h.p;
  void spawn(reader(new Holder(h.p)), { worker: 3 });
  PARKED.add(v as i64);
  return v + (await d.p);
}

async function main() {
  const n: i64 = 20;
  for (let r: i64 = 0; r < n; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  while (PARKED.load() < 2 * n || READ.load() < 2 * n) {
    await yieldNow();
  }
  for (let i = 0; i < 50; i++) {
    await yieldNow();
  }
  KEPT.length = 0;
  console.log(`parked ${PARKED.load()}, read ${READ.load()}`);
}
"#;

/// The program of [`a_finished_promise_shared_after_it_let_go_with_its_original_handed_over_is_freed`].
const SHARE_ORIG_HANDED: &str = r#"// #3498 review: a promise that finished, and so let go of its task, is shared after on that
// task, and its original holder is handed to a task on worker 3, which reads it there after the
// sharing task has read and dropped its copy. The original's drop is the last, and it frees the
// hub: the hub took the node for another task's (`AWAY`), as a finished node has none. Taken for
// a node of this task (`HUB_HERE`), that drop would leave the free to a finish that already ran.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const READ = new Atomic<i64>(0);
const DONE = new Atomic<i64>(0);

async function soon(): Promise<number> {
  await yieldNow();
  return 2;
}

async function reader(h: Holder): Promise<void> {
  for (let i = 0; i < 10; i++) {
    await yieldNow();
  }
  READ.add((await h.p) as i64);
}

async function maker(): Promise<void> {
  const f = soon();
  for (let i = 0; i < 5; i++) {
    await yieldNow();
  }
  const h = new Holder(f);
  const h2 = new Holder(h.p);
  void spawn(reader(h), { worker: 3 });
  READ.add((await h2.p) as i64);
  DONE.add(1);
}

async function main() {
  const n: i64 = 20;
  for (let r: i64 = 0; r < n; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  while (DONE.load() < n || READ.load() < 4 * n) {
    await yieldNow();
  }
  for (let i = 0; i < 50; i++) {
    await yieldNow();
  }
  console.log(`read ${READ.load()}`);
}
"#;

/// The program of [`a_finished_promise_shared_into_module_state_is_dropped_off_its_task`].
const SHARE_KEPT: &str = r#"// #3498 review: a promise that finished, and so let go of its task, is shared into two holders
// kept in module state, which `main` drops one at a time, off the task and under the module lock.
// Its hub is not taken for a hub on the task (it has none), so no drop tries to list it there,
// and every task is found parked for ever.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const KEPT: Holder[] = [];
const SHARED: Holder[] = [];
const PARKED = new Atomic<i64>(0);

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function soon(): Promise<number> {
  await yieldNow();
  return 3;
}

async function maker(): Promise<number> {
  const d = new Holder(dead());
  KEPT.push(new Holder(d.p));
  const f = soon();
  for (let i = 0; i < 5; i++) {
    await yieldNow();
  }
  const h = new Holder(f);
  SHARED.push(h);
  SHARED.push(new Holder(h.p));
  PARKED.add(1);
  return await d.p;
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
  while (SHARED.length > 0) {
    SHARED.pop();
    await yieldNow();
  }
  KEPT.length = 0;
  console.log(`parked ${PARKED.load()}`);
}
"#;

/// The program of [`promises_handed_over_pending_and_awaited_or_dropped_there_as_they_finish`].
const HANDED_ALL: &str = r#"// #3498 review, item 1: promises of a task on worker 1, handed to tasks on worker 2 while still
// pending, one at a time: one is awaited there through `Promise.all`, the next dropped unawaited.
// Each waits for its gate, which the task it was handed to opens right before its await or its
// drop: with `VELT_SCHED_TASK_PAUSE`, the promise finishes, and lets go of its task, between that
// reader's load of its flags and its load of its task. Every value arrives, and nothing is left.

class Box {
  v: i64;
  constructor(v: i64) {
    this.v = v;
  }
}

const GATE = new Atomic<i64>(0);
const DONE = new Atomic<i64>(0);
const GOT = new Atomic<i64>(0);
const DROPPED = new Atomic<i64>(0);

async function opener(i: i64): Promise<number> {
  while (GATE.load() < i) {
    await yieldNow();
  }
  return 1;
}

// Suspended, and not queued, until its gate opens: it waits for a task on worker 3.
async function gated(i: i64): Promise<Box> {
  const v = await spawn(opener(i), { worker: 3 });
  DONE.add(1);
  return new Box(v as i64);
}

async function viaAll(p: Promise<Box>, i: i64): Promise<void> {
  GATE.store(i);
  // Not an array literal: the join polls it with a waker of its own, which maps to no task.
  const ps: Promise<Box>[] = [p];
  const rs = await Promise.all(ps);
  GOT.add(rs[0].v);
}

async function dropper(p: Promise<Box>, i: i64): Promise<void> {
  DROPPED.add(1);
  GATE.store(i);
}

async function maker(i: i64): Promise<void> {
  if (i % 2 === 0) {
    await spawn(viaAll(gated(i), i), { worker: 2 });
  } else {
    await spawn(dropper(gated(i), i), { worker: 2 });
  }
}

async function main() {
  const n: i64 = 60;
  for (let i: i64 = 1; i <= n; i++) {
    await spawn(maker(i), { worker: 1 });
  }
  while (DONE.load() < n) {
    await yieldNow();
  }
  for (let y = 0; y < 100; y++) {
    await yieldNow();
  }
  console.log(`got ${GOT.load()}, dropped ${DROPPED.load()}`);
}
"#;

/// The program of [`a_shared_promise_whose_first_waiter_leaves_after_it_finished_still_wakes_the_next`].
const AWAY_FIRST_LEAVES: &str = r#"// #3498 review, item 1: a promise of a task on worker 1 is handed to a task on worker 2, which
// shares it there (its hub is `AWAY`: the promise is another task's). The first holder to wait, in
// a race, leaves the hub's waiters when a quick promise wins; that quick promise opened the shared
// one's gate, so with `VELT_SCHED_TASK_PAUSE` the shared promise finishes, and lets go of its task,
// before the hub hands its watch to the next waiter, a `Promise.all` holder: that one still gets
// the value. One promise at a time.

class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const GATE = new Atomic<i64>(0);
const GOT = new Atomic<i64>(0);

async function gated(i: i64): Promise<number> {
  while (GATE.load() < i) {
    await yieldNow();
  }
  return 1;
}

async function quick(i: i64): Promise<number> {
  await yieldNow();
  GATE.store(i);
  return 0;
}

async function firstOf(h: Holder, i: i64): Promise<number> {
  return await Promise.race([h.p, quick(i)]);
}

async function behind(h: Holder): Promise<void> {
  // Not an array literal: the join waits in the hub with a waker of its own, of no task.
  const ps: Promise<number>[] = [h.p];
  const rs = await Promise.all(ps);
  GOT.add(rs[0] as i64);
}

async function sharer(p: Promise<number>, i: i64): Promise<void> {
  const h = new Holder(p);
  // Started now: its race's holder is the hub's first waiter, `behind`'s the second.
  const first = firstOf(h, i);
  const b = behind(new Holder(h.p));
  await first;
  await b;
}

async function maker(i: i64): Promise<void> {
  await spawn(sharer(gated(i), i), { worker: 2 });
}

async function main() {
  const n: i64 = 30;
  for (let i: i64 = 1; i <= n; i++) {
    await spawn(maker(i), { worker: 1 });
  }
  console.log(`got ${GOT.load()}`);
}
"#;
