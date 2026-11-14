//! The end of a program under the executor in Velt while a wake by value is still giving its
//! reference back (#2966). A runtime timer thread wakes the last started promise of `main`'s
//! task; the task may then run on a worker, finish, and let worker 0 end the program before the
//! waking thread has released the node and, through it, the task. Those two blocks were counted
//! as leaked now and then (`VELT_RC_STATS=1`), more often on a busy machine. Worker 0 now waits
//! for the wakes under way (std/lang/hooks/sched_wake.velt `settleWakes`).
//!
//! Many copies run at once, so their threads compete for the cores as on a loaded machine: with
//! the wait left out, a few dozen of the runs below leave 2 blocks unfreed on Linux.

use std::path::Path;
use std::process::{Child, Stdio};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

/// The program: a synchronous `main` that starts promises and a timer.
const SRC: &str = include_str!("../../../tests/golden/lang/promises_sync_main.velt");

/// What it prints, as Node does.
const WANT: &str = include_str!("../../../tests/golden/lang/promises_sync_main.out");

/// Copies run at once, and rounds of them.
const AT_ONCE: usize = 12;
const ROUNDS: usize = 40;

/// The `blocks=A/F` of a run's `rc stats` line (a debug runtime's), if it has one.
fn blocks(err: &str) -> Option<(u64, u64)> {
    let stats = err.lines().find(|l| l.starts_with("rc stats:"))?;
    let b = stats
        .split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))?;
    let (a, f) = b.split_once('/')?;
    Some((a.parse().ok()?, f.parse().ok()?))
}

fn start(exe: &Path) -> Child {
    command(exe)
        .env("VELT_THREADS", "8")
        .env("VELT_RC_STATS", "1")
        .env_remove("VELT_SCHEDULER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn a_wake_giving_its_reference_back_at_the_end_frees_every_block() {
    if !cfg!(any(target_os = "macos", target_os = "linux", windows)) {
        return;
    }
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("sync_main.velt");
    std::fs::write(&file, SRC).unwrap();
    let exe = dir.path().join(if cfg!(windows) {
        "sync_main.exe"
    } else {
        "sync_main"
    });
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut unbalanced = vec![];
    for round in 0..ROUNDS {
        let children: Vec<Child> = (0..AT_ONCE).map(|_| start(&exe)).collect();
        for (i, child) in children.into_iter().enumerate() {
            let o = child.wait_with_output().unwrap();
            let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            assert!(o.status.success(), "round {round}, copy {i}: {out}{err}");
            assert_eq!(out, WANT, "round {round}, copy {i}");
            // Only a debug runtime counts blocks.
            if let Some((a, f)) = blocks(&err) {
                if a != f {
                    unbalanced.push(format!("round {round}, copy {i}: blocks={a}/{f}"));
                }
            }
        }
    }
    assert!(
        unbalanced.is_empty(),
        "{} of {} runs left blocks unfreed:\n{}",
        unbalanced.len(),
        ROUNDS * AT_ONCE,
        unbalanced.join("\n")
    );
}
