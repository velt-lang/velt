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
    std::fs::write(dir.path().join("main.vlt"), CELL).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir.path())
        .output()
        .unwrap();
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
    let debug = velt(&dir, &["build", "main.vlt", "--emit", "vir"]);
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
    let release = velt(&dir, &["build", "--release", "main.vlt", "--emit", "vir"]);
    let release = String::from_utf8_lossy(&release.stdout);
    assert!(!release.contains("velt_rt_cell_"), "{release}");
}

#[test]
fn a_cell_used_by_one_task_runs() {
    let dir = test_dir::TestDir::new();
    let out = velt(&dir, &["run", "main.vlt"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\n");
}
