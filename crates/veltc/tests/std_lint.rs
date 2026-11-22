//! The tests of scripts/std-lint's own rules (`velt test scripts/std-lint`): the `// SAFETY:`
//! rule (scripts/std-lint/safety.velt, #3496), which `cargo xtask precheck` runs on the std files
//! a change touches. veltc's own check of the same rule, over std's parsed modules, is
//! `native::unsafe_sites::std_tests`.

use std::path::Path;

mod no_window;
mod runtime_support;
mod test_dir;

#[test]
fn std_lint_rules_pass_their_tests() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let home = test_dir::TestDir::new();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["test", "--no-cache", "scripts/std-lint/safety.test.velt"])
        .current_dir(&root)
        .env("VELT_HOME", home.path().join("home"))
        .output()
        .expect("run velt test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains(" 0 failed") && !stdout.contains(" 0 passed"),
        "velt test scripts/std-lint/safety.test.velt: exit {:?}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}
