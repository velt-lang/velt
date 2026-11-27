//! The secret-dependent code of std/crypto is constant time in the machine code:
//! `scripts/ct-counts.py` builds each probe in release mode and runs it under valgrind, once
//! counting its instructions for a set of secrets (a second count is a loop or an uneven branch
//! on the secret) and once under memcheck with the secret marked undefined (which reports every
//! branch and every address that depends on it, also where both sides cost the same). The source
//! need not show either: LLVM turns a mask it can see through into a branch, and no functional
//! test notices (#2392).
//!
//! Runs where valgrind, python3 and a C compiler are installed, on x86-64 Linux; elsewhere it
//! says so and passes. With `VELT_CT_TESTS` set (the `ct-counts` job of ci.yml and nightly.yml,
//! which installs valgrind: crates/xtask/src/ct_counts.rs) a missing tool fails the test instead,
//! so a job that loses valgrind goes red rather than passing with nothing measured (#3942).

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

/// Whether `VELT_CT_TESTS` asks for the checks: set to anything but empty or `0`.
fn required(value: Option<&OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// Skips the check with `note`, or fails it where `VELT_CT_TESTS` requires it.
fn skip(note: &str) {
    if required(std::env::var_os("VELT_CT_TESTS").as_deref()) {
        panic!("VELT_CT_TESTS is set, but {note}");
    }
    eprintln!("note: {note}; skipping");
}

fn has(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn run(args: &[&str]) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Command::new("python3")
        .arg(root.join("scripts/ct-counts.py"))
        .args(args)
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

fn can_run() -> bool {
    let here = cfg!(all(target_os = "linux", target_arch = "x86_64"));
    if here && has("valgrind") && has("python3") && has("cc") {
        return true;
    }
    skip("the check needs valgrind, python3 and cc on x86-64 Linux");
    false
}

#[test]
fn secrets_do_not_change_the_instructions_executed() {
    if can_run() {
        run(&["--runs", "20"]);
    }
}

/// Memcheck needs the C library's symbols (it replaces `memcmp`, `strlen` and their kin), and
/// where they are stripped it stops before the program starts ("a function redirection which is
/// mandatory ... cannot be set up"). The shim needs memcheck's header. The instruction count
/// needs neither.
fn memcheck_starts() -> bool {
    let starts = Command::new("valgrind")
        .args(["--tool=memcheck", "-q", "true"])
        .output()
        .is_ok_and(|o| o.status.success());
    let header = Command::new("cc")
        .args([
            "-fsyntax-only",
            "-include",
            "valgrind/memcheck.h",
            "-x",
            "c",
            "/dev/null",
        ])
        .output()
        .is_ok_and(|o| o.status.success());
    if !starts || !header {
        skip(
            "valgrind's memcheck cannot start a program here (libc6-dbg), or its header is missing",
        );
    }
    starts && header
}

#[test]
fn ct_tests_is_on_for_anything_but_empty_or_zero() {
    assert!(required(Some(OsStr::new("1"))));
    assert!(required(Some(OsStr::new("yes"))));
    assert!(!required(Some(OsStr::new("0"))));
    assert!(!required(Some(OsStr::new(""))));
    assert!(!required(None));
}

#[test]
fn no_branch_or_address_depends_on_a_secret() {
    if can_run() && memcheck_starts() {
        run(&["--memcheck"]);
    }
}
