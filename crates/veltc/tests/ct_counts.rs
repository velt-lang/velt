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

/// `program` compiled with `cc` and `args` into `out`, or the compiler's output.
fn cc(program: &Path, out: &Path, args: &[&str]) -> Result<(), String> {
    let made = Command::new("cc")
        .args(args)
        .arg("-o")
        .arg(out)
        .arg(program)
        .output()
        .map_err(|e| e.to_string())?;
    match made.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&made.stderr).into_owned()),
    }
}

/// What scripts/ct-shim.c prints when the probe's other threads never settled (ct-counts.py's
/// `UNSETTLED`).
const UNSETTLED: &str = "ct-shim: the other threads never settled before the count started";

/// scripts/ct-shim.c and tests/ct_counts/thread.c built into a temporary directory with a
/// secret and a stop file, or `None` (skipped) where the shim doesn't build.
fn thread_probe() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    if !can_run() {
        return None;
    }
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let shim = dir.path().join("ct-shim.so");
    let made = cc(
        &root.join("../../scripts/ct-shim.c"),
        &shim,
        &["-shared", "-fPIC", "-O1", "-ldl"],
    );
    if let Err(e) = made {
        skip(&format!(
            "scripts/ct-shim.c doesn't build (valgrind's headers?): {e}"
        ));
        return None;
    }
    let probe = root.join("tests/ct_counts/thread.c");
    cc(&probe, &dir.path().join("thread"), &["-O1", "-pthread"]).expect("thread.c builds");
    std::fs::write(dir.path().join("secret.bin"), [7u8; 32]).expect("the secret is written");
    std::fs::write(dir.path().join("secret.bin.stop"), b"").expect("the stop file is written");
    Some((dir, shim))
}

/// What scripts/ct-shim.c counts of tests/ct_counts/thread.c, with `thread` as its `CT_THREAD`
/// (no second thread without one), and valgrind's stderr.
fn thread_probe_count(dir: &Path, shim: &Path, thread: Option<&str>) -> (u64, String) {
    let secret = dir.join("secret.bin");
    let stop = dir.join("secret.bin.stop");
    let mut cmd = Command::new("valgrind");
    // With fair scheduling valgrind hands its one running thread's turn to the waiting thread
    // when the turn ends, so a thread still running when the count starts always gets turns
    // inside it; with valgrind's default it only sometimes does, as on a busy machine.
    cmd.args([
        "--tool=cachegrind",
        "--fair-sched=yes",
        "--cache-sim=no",
        "--cachegrind-out-file=/dev/null",
        "--instr-at-start=no",
    ])
    .arg(dir.join("thread"))
    .args([&secret, &stop])
    .env("LD_PRELOAD", shim)
    .env("VELT_CT_SECRET", &secret)
    .env("VELT_CT_STOP", &stop)
    .env_remove("CT_THREAD");
    if let Some(t) = thread {
        cmd.env("CT_THREAD", t);
    }
    let out = cmd.output().expect("valgrind runs");
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the thread probe failed:\n{text}");
    let refs = text
        .lines()
        .find_map(|l| l.split("I refs:").nth(1))
        .unwrap_or_else(|| panic!("no count in:\n{text}"));
    let n = refs.trim().replace(',', "").parse().expect("a count");
    (n, text)
}

/// What is counted is the code under test, not the work of a thread that started just before
/// it: velt_rt's host thread starts there in a test probe, and where valgrind ran some of its
/// start inside the region, one secret's count changed from run to run on a busy machine, which
/// ct-counts.py reported as a count that depends on the secret (#3942). The shim waits for the
/// other threads to stop running first. The probe's second thread works about 10M instructions,
/// then waits; with it, the count may differ by what the C library does in a process with
/// threads (its system call wrappers), a few hundred instructions, never by that work.
#[test]
fn a_thread_started_before_the_secret_is_not_counted() {
    let Some((dir, shim)) = thread_probe() else {
        return;
    };
    let (alone, _) = thread_probe_count(dir.path(), &shim, None);
    for _ in 0..3 {
        let (with, text) = thread_probe_count(dir.path(), &shim, Some("1"));
        assert!(!text.contains(UNSETTLED), "{text}");
        assert!(
            with.abs_diff(alone) < 1_000,
            "the count with a second thread is {with}, without it {alone}: the thread's work \
             was counted"
        );
    }
}

/// A thread that never stops running can't be waited for: the shim gives up after two seconds
/// and says so, and ct-counts.py runs the secret again, then fails as that, never as a count
/// that depends on the secret.
#[test]
fn a_thread_that_never_waits_is_reported() {
    let Some((dir, shim)) = thread_probe() else {
        return;
    };
    let (_, text) = thread_probe_count(dir.path(), &shim, Some("spin"));
    assert!(
        text.contains(UNSETTLED),
        "no report of the unsettled start:\n{text}"
    );
}
