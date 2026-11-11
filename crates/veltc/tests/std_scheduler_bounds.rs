//! The executor in Velt with more than one worker, or against a copy of std with a module added:
//! each program is built with `VELT_SCHEDULER=std` only and checks one thing of that build (the
//! worker count's bound, a latch's buffer of wakers). Split from std_scheduler.rs by size.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Workers start only where `#sched-thread` has threads (macOS and Linux for now).
fn has_threads() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// `src` built with `VELT_SCHEDULER=std` (against the std in `std_dir`, or the repository's) and
/// run with each `(VELT_THREADS, VELT_RT_DEBUG_ALLOC)` of `runs`: exit code and output of each.
fn std_runs(name: &str, src: &str, std_dir: Option<&Path>, runs: &[(&str, &str)]) -> Vec<String> {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir
        .path()
        .join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
    let mut build = command(env!("CARGO_BIN_EXE_velt"));
    build
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .env("VELT_SCHEDULER", "std");
    if let Some(std_dir) = std_dir {
        build.env("VELT_STD", std_dir);
    }
    let o = build.output().unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    runs.iter()
        .map(|(threads, debug_alloc)| {
            let o = command(&exe)
                .env("VELT_THREADS", threads)
                .env("VELT_RT_DEBUG_ALLOC", debug_alloc)
                .env_remove("VELT_SCHEDULER")
                .env_remove("VELT_RESCUE")
                .output()
                .unwrap();
            format!(
                "{:?} {}{}",
                o.status.code(),
                String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .collect()
}

/// `VELT_THREADS` above 4096 is taken for a typo, like one that is not a number: the online
/// cores. The bound is checked after each digit (5000 got through when it was checked before
/// the last one).
#[test]
fn velt_threads_past_the_bound_runs_on_the_cores() {
    if !has_threads() {
        return;
    }
    let src = r#"import { workerCount } from "velt/task";

async function main() {
  console.log(`workers: ${workerCount()}`);
}
"#;
    let runs = std_runs(
        "worker_bound",
        src,
        None,
        &[
            ("x", "0"),
            ("5000", "0"),
            ("4097", "0"),
            ("40960", "0"),
            ("8", "0"),
        ],
    );
    assert_eq!(runs[4], "Some(0) workers: 8\n", "VELT_THREADS=8");
    assert!(!runs[0].contains("workers: 5000"), "{}", runs[0]);
    for (run, v) in runs[1..4].iter().zip(["5000", "4097", "40960"]) {
        assert_eq!(
            run, &runs[0],
            "VELT_THREADS={v} runs on the cores, as one not a number"
        );
    }
}

/// A latch opened with many waiters of other tasks while several workers run (sched_latch.velt
/// `openAs`): their wakers go to a buffer grown 4, 8, 16, 32, 64 and freed after each growth and
/// at the end, each free with the size the block was made with, which the debug runtime's
/// allocator checks (`VELT_RT_DEBUG_ALLOC=1` aborts on a wrong size). No std caller has two
/// waiters on one latch (a promise's settlement waits once), so the program reaches the latch
/// through a module added to a copy of std.
#[test]
fn a_latch_with_many_waiters_frees_its_waker_buffer_by_size() {
    if !has_threads() {
        return;
    }
    let dir = test_dir::TestDir::new();
    let std_dir = dir.path().join("std");
    copy_dir(&manifest().join("../../std"), &std_dir);
    std::fs::create_dir_all(std_dir.join("latch_probe")).unwrap();
    std::fs::write(std_dir.join("latch_probe/index.velt"), LATCH_PROBE).unwrap();
    let runs = std_runs(
        "latch_many",
        LATCH_MANY,
        Some(&std_dir),
        &[("4", "1"), ("1", "1")],
    );
    for (run, threads) in runs.iter().zip(["4", "1"]) {
        assert_eq!(run, "Some(0) sum 2080\n", "VELT_THREADS={threads}");
    }
}

/// Copies directory `from` to `to` (made), recursively.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// The module of [`a_latch_with_many_waiters_frees_its_waker_buffer_by_size`]'s std copy: the
/// runtime's latch entries (which only std may declare).
const LATCH_PROBE: &str = r#"declare function velt_rt_latch_new(): u64;
declare function velt_rt_latch_free(latch: u64);
declare function velt_rt_latch_open(latch: u64);
declare async function velt_rt_latch_wait(latch: u64): Promise<void>;

export function latchNew(): u64 {
  return velt_rt_latch_new();
}

export function latchOpen(l: u64): void {
  velt_rt_latch_open(l);
}

export function latchFree(l: u64): void {
  velt_rt_latch_free(l);
}

export async function latchWait(l: u64): Promise<void> {
  await velt_rt_latch_wait(l);
}
"#;

/// 64 tasks on worker 0 wait on one latch: each sends on `ready` and starts its wait in the same
/// poll, so all wait before `main` (woken by the last send) opens it.
const LATCH_MANY: &str = r#"import { channel, Channel } from "velt/sync";
import { latchFree, latchNew, latchOpen, latchWait } from "velt/latch_probe";

async function waiter(l: u64, ready: Channel<i64>, i: i64): Promise<i64> {
  ready.trySend(i);
  await latchWait(l);
  return i + 1;
}

async function main() {
  const l = latchNew();
  const ready = channel<i64>(64);
  const tasks: Promise<i64>[] = [];
  for (let i = 0; i < 64; i++) {
    tasks.push(spawn(waiter(l, ready, i), { worker: 0 }));
  }
  for (let i = 0; i < 64; i++) {
    await ready.receive();
  }
  latchOpen(l);
  let sum = 0;
  for (const t of tasks) sum += await t;
  latchFree(l);
  console.log(`sum ${sum}`);
}
"#;
