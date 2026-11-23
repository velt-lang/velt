//! A join handle dropped on one worker while its task, on another, parks for ever (#3438,
//! std/lang/hooks/sched_join.velt `dropRunning`, sched_task_lost.velt `counts`). The drop
//! clears the handle's bit and gives back its reference; a park that saw the bit gone but the
//! reference still there took the task for one with a reference more than its promises, with
//! no handle left to wake it, and the task, never dropped, kept its 7 blocks to the end. That
//! needed the drop's thread preempted in a few instructions' window (a few runs in a thousand
//! under load); here the window is held open for 50 ms (`VELT_SCHED_HANDLE_PAUSE`, which only a
//! counted run reads, sched_debug.velt), and every run leaked before the fix.

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

/// Builds `src` and runs it `runs` times at 4 and 8 workers, counted, with the window `pause`
/// held open: exit 0, "ok" printed, every block freed.
fn check(name: &str, src: &str, pause: &str, runs: usize) {
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
                .env("VELT_SCHED_HANDLE_PAUSE", pause)
                .env_remove("VELT_RESCUE")
                .env_remove("VELT_SCHEDULER")
                .output()
                .unwrap();
            let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
            let err = String::from_utf8_lossy(&o.stderr);
            if o.status.code() != Some(0) || out != "ok\n" || !all_freed(&err) {
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

/// `main` drops the handle of a task it spawned on worker 1 (`void spawn`) while the task parks
/// for ever: the drop pauses after the slot's unlock, where it gave the reference back before.
#[test]
fn a_task_whose_handle_is_dropped_as_it_parks_is_dropped() {
    check("handle_dropped", HANDLE_DROPPED, "drop", 3);
}

/// `main` holds the handle and drops it after the task parked: the park pauses between reading
/// the slot's word and the count, where the drop came in between before.
#[test]
fn a_task_whose_handle_goes_between_its_park_reads_is_dropped() {
    check("handle_between", HANDLE_BETWEEN, "count", 3);
}

const HANDLE_DROPPED: &str = r#"async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function maker(): Promise<number> {
  return await dead();
}

async function main() {
  void spawn(maker(), { worker: 1 });
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  console.log("ok");
}
"#;

const HANDLE_BETWEEN: &str = r#"async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function maker(): Promise<number> {
  return await dead();
}

async function hold(): Promise<void> {
  const h = spawn(maker(), { worker: 1 });
  for (let y = 0; y < 2000; y++) {
    await yieldNow();
  }
}

async function main() {
  await hold();
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  console.log("ok");
}
"#;
