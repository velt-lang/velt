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
    velt_src(dir, args, scheduler, CELL)
}

/// [`velt_on`] for program `src`.
fn velt_src(
    dir: &test_dir::TestDir,
    args: &[&str],
    scheduler: Option<&str>,
    src: &str,
) -> std::process::Output {
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
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

/// [`CELL`] in a program that runs on the executor (it awaits).
const CELL_ASYNC: &str = "async function main() {
  let last = \"\";
  const f = (v: string) => { last = v; };
  const fs = [f];
  fs[0](\"a\");
  await sleep(0);
  console.log(last);
}
";

/// #2832: under the executor in Velt the check is std's (std/lang/hooks/sched_cells.velt), keyed
/// by its task ids: debug builds of a program that runs on it call its hooks instead of the
/// runtime's, release builds neither. A program that runs nothing on it doesn't load the executor
/// (#2649 P3(a)) and keeps the runtime's check.
#[test]
fn debug_builds_under_std_report_cells_to_std() {
    let dir = test_dir::TestDir::new();
    let plain = velt_on(&dir, &["build", "main.velt", "--emit", "vir"], Some("std"));
    let plain = String::from_utf8_lossy(&plain.stdout);
    assert!(!plain.contains("schedCell"), "{plain}");
    assert_eq!(
        plain.contains("velt_rt_cell_new("),
        cfg!(debug_assertions),
        "{plain}"
    );
    let debug = velt_src(
        &dir,
        &["build", "main.velt", "--emit", "vir"],
        Some("std"),
        CELL_ASYNC,
    );
    let debug = String::from_utf8_lossy(&debug.stdout);
    let checked = cfg!(debug_assertions);
    for hook in ["schedCellNew", "schedCellUse", "schedCellFree"] {
        assert_eq!(debug.contains(hook), checked, "{hook}:\n{debug}");
    }
    assert!(!debug.contains("velt_rt_cell_"), "{debug}");
    let release = velt_src(
        &dir,
        &["build", "--release", "main.velt", "--emit", "vir"],
        Some("std"),
        CELL_ASYNC,
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

/// Copies directory `from` into `to` (made), recursively.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Runs std test file `src` from a copy of std (as `lang/hooks/<name>.test.velt`) with `threads`
/// workers: the runner's exit code, and its output (stdout, then stderr). The runner runs each
/// test in a process of its own and reports how it ended.
fn std_white_box(name: &str, src: &str, threads: &str) -> (Option<i32>, String) {
    let dir = test_dir::TestDir::new();
    let std_copy = dir.path().join("std");
    copy_tree(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std_copy,
    );
    let test = std_copy.join(format!("lang/hooks/{name}.test.velt"));
    std::fs::write(&test, src).unwrap();
    let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .arg("test")
        .arg(&test)
        .current_dir(dir.path())
        .env("VELT_STD", &std_copy)
        .env_remove("VELT_SCHEDULER")
        .env("VELT_THREADS", threads)
        .output()
        .unwrap();
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (o.status.code(), out)
}

/// The runner's line for a test that called `abort` (SIGABRT).
const ABORTED: &str = "(exit code 134)";

/// #2832: std's check (sched_cells.velt) stops a program whose second task uses a cell another
/// task owns, with the runtime's message, after what it printed. White-box, from a copy of std,
/// under the default executor: a std test file importing the executor's hook modules doesn't
/// build under `VELT_SCHEDULER=std` yet (#2875), and there `taskId()` gives the runtime's ids, so
/// the hooks run as under std, keyed by the runtime's tasks.
#[test]
fn std_check_stops_a_second_task_using_a_cell() {
    if !cfg!(debug_assertions) || !cfg!(unix) {
        return;
    }
    let src = r#"import { describe, it } from "velt/test";
import { schedCellNew, schedCellUse } from "./sched_cells";

describe("the cell check", () => {
  it("stops a second task using a cell", async () => {
    const c: usize = 0x5000;
    // The task making the cell owns it (no use claims it).
    await spawn((async () => {
      schedCellNew(c, "läst");
    })());
    console.log("before the second task");
    await spawn((async () => {
      schedCellUse(c);
    })());
    console.log("not reached");
  });
});
"#;
    for threads in ["1", "4"] {
        let (code, out) = std_white_box("cells_two_tasks", src, threads);
        assert_eq!(code, Some(1), "T{threads}: {out}");
        assert!(out.contains(ABORTED), "T{threads}: {out}");
        assert!(
            out.contains("before the second task\n"),
            "T{threads}: {out}"
        );
        assert!(!out.contains("not reached"), "T{threads}: {out}");
        for part in [
            "velt debug-cells: captured variable `läst` was used by two tasks: it belongs to \
             runtime task ",
            "(on a thread outside the workers), and runtime task ",
            "used it too. A closure that assigns a variable it captured was shared with another \
             task or HTTP request instead of copied",
        ] {
            assert!(out.contains(part), "T{threads}: {part:?} not in {out}");
        }
    }
}

/// #2832: a cell guarded by a use under the module lock stops a task that uses it without the
/// lock (see [`std_check_stops_a_second_task_using_a_cell`] for why white-box).
#[test]
fn std_check_stops_an_unlocked_use_of_a_guarded_cell() {
    if !cfg!(debug_assertions) || !cfg!(unix) {
        return;
    }
    let src = r#"import { describe, it } from "velt/test";
import { cellNew, cellUse, schedCellUse } from "./sched_cells";

describe("the cell check", () => {
  it("stops an unlocked use of a guarded cell", async () => {
    const c: usize = 0x6000;
    cellNew(c, "hits", 7);
    // A second task's use under the module lock guards it.
    cellUse(c, 8, true);
    console.log("guarded");
    await spawn((async () => {
      schedCellUse(c);
    })());
    console.log("not reached");
  });
});
"#;
    for threads in ["1", "4"] {
        let (code, out) = std_white_box("cells_unlocked", src, threads);
        assert_eq!(code, Some(1), "T{threads}: {out}");
        assert!(out.contains(ABORTED), "T{threads}: {out}");
        // The guarded rule's message, not the two-task one.
        assert!(!out.contains("was used by two tasks"), "T{threads}: {out}");
        assert!(out.contains("guarded\n"), "T{threads}: {out}");
        assert!(!out.contains("not reached"), "T{threads}: {out}");
        for part in [
            "velt debug-cells: captured variable `hits` is shared through module state, and \
             runtime task ",
            "(on a thread outside the workers) used it without the module lock",
        ] {
            assert!(out.contains(part), "T{threads}: {part:?} not in {out}");
        }
    }
}
