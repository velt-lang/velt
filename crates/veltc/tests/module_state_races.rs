//! One module state per process (#76): the golden `lang/module_let_parallel` (tasks on every
//! core updating one module counter, one module `Map` and objects in it) runs many times under
//! both schedulers, and every run must count every update. A lost update or a race on the
//! objects' reference counts shows as other totals, a crash or a leak. The same program run by
//! `node` (when installed, with `spawn` and the task functions as plain promises) prints the
//! same totals. `lang/module_let_mutex` (tasks blocking on a `Mutex` while others replace the
//! module array they iterate) runs many times too: the array must never be freed under them.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

const GOLDEN: &str = "../../tests/golden/lang/module_let_parallel";

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

const MUTEX_GOLDEN: &str = "../../tests/golden/lang/module_let_mutex";

fn expected() -> String {
    expected_of(GOLDEN)
}

fn expected_of(golden: &str) -> String {
    std::fs::read_to_string(manifest().join(format!("{golden}.out")))
        .unwrap()
        .replace("\r\n", "\n")
}

fn stdout(o: &std::process::Output) -> String {
    assert!(
        o.status.success(),
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

#[test]
fn every_update_counts_under_both_schedulers() {
    runs_the_same_every_time(GOLDEN, 8);
}

#[test]
fn a_mutex_wait_keeps_the_module_lock() {
    runs_the_same_every_time(MUTEX_GOLDEN, 4);
}

/// Build golden `golden` and run it `runs` times under each scheduler: every run prints its
/// `.out`.
fn runs_the_same_every_time(golden: &str, runs: u32) {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let src = manifest().join(format!("{golden}.vlt"));
    let exe = dir
        .path()
        .join(if cfg!(windows) { "prog.exe" } else { "prog" });
    let build = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .output()
        .unwrap();
    stdout(&build);
    let want = expected_of(golden);
    for scheduler in ["per-core", "stealing"] {
        for run in 0..runs {
            let out = command(&exe)
                .env("VELT_SCHEDULER", scheduler)
                .output()
                .unwrap();
            assert_eq!(stdout(&out), want, "{golden}: {scheduler}, run {run}");
        }
    }
}

/// What Velt provides as globals, as Node promises: one thread, so `spawn` is the promise itself.
const NODE_SHIM: &str = "function spawn<T>(p: Promise<T>, _o?: { worker: number }): Promise<T> {
  return p;
}
const workerCount = (): number => 4;
const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));
const yieldNow = (): Promise<void> => new Promise((r) => setImmediate(r));
";

#[test]
fn node_prints_the_same_totals() {
    let src = std::fs::read_to_string(manifest().join(format!("{GOLDEN}.vlt"))).unwrap();
    // The shim stands for the `velt:` imports.
    let src: String = src
        .lines()
        .filter(|l| !l.starts_with("import "))
        .map(|l| format!("{l}\n"))
        .collect();
    let dir = test_dir::TestDir::new();
    let ts = dir.path().join("parallel.ts");
    std::fs::write(&ts, format!("{NODE_SHIM}{src}\nmain();\n")).unwrap();
    let Ok(node) = command("node")
        .arg("--experimental-strip-types")
        .arg(&ts)
        .output()
    else {
        return; // no node
    };
    if !node.status.success() {
        let err = String::from_utf8_lossy(&node.stderr);
        // A Node without type stripping.
        assert!(
            err.contains("strip-types") || err.contains("bad option"),
            "{err}"
        );
        return;
    }
    assert_eq!(stdout(&node), expected());
}
