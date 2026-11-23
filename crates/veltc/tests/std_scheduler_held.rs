//! A task parked for ever while its frames hold a promise that already finished (#3498,
//! std/lang/hooks/sched_node_done.velt): such a promise can never wake the task, but it kept its
//! reference to the task until it was freed, so the park's count had one reference too many and
//! the task, with what it holds, stayed to the end of the program. A finished promise now gives
//! that reference back as it finishes, when nothing but its owner (or its hub) can reach it. Each
//! program runs counted with the checking allocator (`VELT_RC_STATS=1`, `VELT_RT_DEBUG_ALLOC=1`)
//! at 4 and 8 workers: exit 0, its output, and every block freed.

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

/// The root waits for ever while its frame holds a promise that finished without being
/// awaited (sched_node_done.velt `letGo`). Before, every run left 280 blocks.
#[test]
fn a_task_holding_a_promise_that_finished_unawaited_is_found_parked_for_ever() {
    check("held_plain", HELD_PLAIN, "parked 20\n", 3);
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
/// 340 blocks.
#[test]
fn a_finished_promise_shared_after_it_let_go_of_its_task_is_read() {
    check("share_finished", SHARE_FINISHED, "parked 40, read 40\n", 3);
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
