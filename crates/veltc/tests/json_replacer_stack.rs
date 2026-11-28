//! The stack a JSON replacer's walk takes per level (std/encoding/json/any_replace.velt): a
//! replacer returning a new value at every level must reach `JSON_CALLBACK_DEPTH`'s `RangeError`
//! before the thread's stack runs out, as tests/golden/std/json_replacer_nested_calls.velt's
//! "endless" case does. Its recursive functions keep what they would inline out of line
//! (`memberOf`, `nextRead`, `jsonReplaced`'s `@noInline`); inlined, a level took about 10% more
//! and the golden overflowed Windows' stack in `--release`. Linux: the golden, built
//! `--release`, runs within a stack of `BUDGET_KB` (it needs about 7,200 KB; 7,800 with the
//! inlining).

#![cfg(target_os = "linux")]

mod no_window;
mod test_dir;

/// The stack the golden may take, in KB: a margin of about 4% over what it needs.
const BUDGET_KB: u32 = 7500;

#[test]
fn a_replacers_endless_walk_fits_its_stack_budget() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let golden = root.join("tests/golden/std/json_replacer_nested_calls");
    let dir = test_dir::TestDir::new();
    let exe = dir.path().join("walk");
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--release", "-o"])
        .arg(&exe)
        .arg(golden.with_extension("velt"))
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "velt build --release: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The soft limit of the stack, lowered for the program alone.
    let run = no_window::command("sh")
        .arg("-c")
        .arg(format!("ulimit -s {BUDGET_KB} && exec \"$0\""))
        .arg(&exe)
        .output()
        .unwrap();
    let want = std::fs::read_to_string(golden.with_extension("out")).unwrap();
    assert!(
        run.status.success() && String::from_utf8_lossy(&run.stdout) == want,
        "the golden needs more than {BUDGET_KB} KB of stack ({:?}): {}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
}
