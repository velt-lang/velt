//! The secret-dependent code of std/crypto executes the same instructions whatever the secret:
//! `scripts/ct-counts.py` builds each probe in release mode and counts its instructions under
//! valgrind for a set of secrets. A second count is a branch or a loop on the secret in the
//! machine code, which the source need not show: LLVM turns a mask it can see through into a
//! branch, and no functional test notices (#2392).
//!
//! Runs where valgrind and python3 are installed, on x86-64 Linux; elsewhere it says so and
//! passes.

use std::path::Path;
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn secrets_do_not_change_the_instructions_executed() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) || !has("valgrind") || !has("python3")
    {
        eprintln!("note: needs valgrind and python3 on x86-64 Linux; skipping");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Command::new("python3")
        .arg(root.join("scripts/ct-counts.py"))
        .args(["--runs", "20"])
        .env("VELT", env!("CARGO_BIN_EXE_velt"))
        .output()
        .expect("scripts/ct-counts.py runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
}
