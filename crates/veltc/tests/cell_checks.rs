//! Debug programs built by a debug compiler report captured variables' cells to the debug
//! runtime, which aborts when two tasks use one (#916; velt_rt `cell_owner.rs`). Release builds
//! emit none of it.

mod no_window;
mod test_dir;

/// A variable assigned by a closure stored in an array: it lives in a cell.
const CELL: &str = "function main() {
  let last = \"\";
  const f = (v: string) => { last = v; };
  const fs = [f];
  fs[0](\"a\");
  console.log(last);
}
";

fn velt(dir: &test_dir::TestDir, args: &[&str]) -> std::process::Output {
    velt_on(dir, args, None)
}

/// [`velt`] with `VELT_SCHEDULER` set to `scheduler` (removed for `None`).
fn velt_on(
    dir: &test_dir::TestDir,
    args: &[&str],
    scheduler: Option<&str>,
) -> std::process::Output {
    std::fs::write(dir.path().join("main.velt"), CELL).unwrap();
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(args).current_dir(dir.path());
    match scheduler {
        Some(s) => cmd.env("VELT_SCHEDULER", s),
        None => cmd.env_remove("VELT_SCHEDULER"),
    };
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn debug_builds_report_cells_and_release_builds_do_not() {
    let dir = test_dir::TestDir::new();
    let debug = velt(&dir, &["build", "main.velt", "--emit", "vir"]);
    let debug = String::from_utf8_lossy(&debug.stdout);
    // The checks follow the compiler's own build: a debug `velt` links the debug runtime.
    let checked = cfg!(debug_assertions);
    for call in [
        "velt_rt_cell_new(",
        "velt_rt_cell_use(",
        "velt_rt_cell_free(",
    ] {
        assert_eq!(debug.contains(call), checked, "{call}:\n{debug}");
    }
    let release = velt(&dir, &["build", "--release", "main.velt", "--emit", "vir"]);
    let release = String::from_utf8_lossy(&release.stdout);
    assert!(!release.contains("velt_rt_cell_"), "{release}");
}

/// #2832: under the executor in Velt the check is std's (std/lang/hooks/sched_cells.velt), keyed
/// by its task ids: debug builds call its hooks instead of the runtime's, release builds neither.
#[test]
fn debug_builds_under_std_report_cells_to_std() {
    let dir = test_dir::TestDir::new();
    let debug = velt_on(&dir, &["build", "main.velt", "--emit", "vir"], Some("std"));
    let debug = String::from_utf8_lossy(&debug.stdout);
    let checked = cfg!(debug_assertions);
    for hook in ["schedCellNew", "schedCellUse", "schedCellFree"] {
        assert_eq!(debug.contains(hook), checked, "{hook}:\n{debug}");
    }
    assert!(!debug.contains("velt_rt_cell_"), "{debug}");
    let release = velt_on(
        &dir,
        &["build", "--release", "main.velt", "--emit", "vir"],
        Some("std"),
    );
    let release = String::from_utf8_lossy(&release.stdout);
    assert!(!release.contains("schedCell"), "{release}");
    assert!(!release.contains("velt_rt_cell_"), "{release}");
}

#[test]
fn a_cell_used_by_one_task_runs() {
    for scheduler in [None, Some("std")] {
        let dir = test_dir::TestDir::new();
        let out = velt_on(&dir, &["run", "main.velt"], scheduler);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "a\n", "{scheduler:?}");
    }
}

/// #2737: a closure stored in module state and called from tasks on several workers shares its
/// cell with the task that stored it; the module lock orders their uses, so the check lets it be.
const MODULE_STATE: &str = "const H: (() => void)[] = [];

function register(): () => number {
  let hits = 0;
  H.push(() => {
    hits += 1;
  });
  return () => hits;
}

async function worker(n: number): Promise<void> {
  for (let i = 0; i < n; i++) {
    H[0]();
    await sleep(0);
  }
}

async function main() {
  const read = register();
  const ts: Promise<void>[] = [];
  for (let t = 0; t < 8; t++) {
    ts.push(spawn(worker(80)));
  }
  await Promise.all(ts);
  console.log(`hits ${read()}`);
}
";

#[test]
fn a_closure_in_module_state_called_from_several_tasks_runs() {
    // Under std too (#2832): its check knows the module lock (sched_module.velt `held`).
    for scheduler in [None, Some("std")] {
        let dir = test_dir::TestDir::new();
        std::fs::write(dir.path().join("main.velt"), MODULE_STATE).unwrap();
        let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
        cmd.args(["run", "main.velt"])
            .env("VELT_THREADS", "4")
            .current_dir(dir.path());
        match scheduler {
            Some(s) => cmd.env("VELT_SCHEDULER", s),
            None => cmd.env_remove("VELT_SCHEDULER"),
        };
        let out = cmd.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{scheduler:?}: {stderr}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "hits 640\n",
            "{scheduler:?}: {stderr}"
        );
    }
}
