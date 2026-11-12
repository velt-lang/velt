//! White-box std tests that only build where the executor in Velt has its system calls (macOS
//! and Linux): their test files import std's scheduler modules (`sched_mem`, `sched_io`, ...),
//! which call POSIX functions a Windows link doesn't have. `velt test std/` would build them on
//! every OS, so they live here (`std_unix/<path in std>`) and each runs from a copy of std with
//! the file put back at its place, as `velt test std/` runs a file next to the code: in a debug
//! build with the block counters and the checking allocator, and in a release build.

#![cfg(unix)]

use std::path::Path;

use crate::{no_window, test_dir};

/// Runs `velt test` on the test file `rel` (a path inside std, as `net/x.test.velt`) with the
/// text `src`, from a copy of the repository's std that has it at `rel`; panics with the report
/// unless at least one test ran and all passed, in both builds.
pub fn velt_test_in_std_copy(rel: &str, src: &str) {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tmp = test_dir::TestDir::new();
    let std_dir = tmp.path().join("std");
    copy_dir(&manifest.join("../../std"), &std_dir);
    let file = std_dir.join(rel);
    std::fs::write(&file, src).unwrap();
    for release in [false, true] {
        let work = tmp.path().join(if release { "release" } else { "debug" });
        std::fs::create_dir_all(&work).unwrap();
        let mut command = no_window::command(env!("CARGO_BIN_EXE_velt"));
        command.arg("test").arg(&file);
        if release {
            command.arg("--release").env_remove("VELT_RC_STATS");
        } else {
            command.env("VELT_RC_STATS", "1");
            if std::env::var_os("VELT_RT_DEBUG_ALLOC").is_none() {
                command.env("VELT_RT_DEBUG_ALLOC", "1");
            }
        }
        let out = command
            .current_dir(&work)
            .env("VELT_STD", &std_dir)
            .env("VELT_HOME", tmp.path().join("home"))
            .env_remove("CLICOLOR_FORCE")
            .env("NO_COLOR", "1")
            .output()
            .expect("run velt test");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let ran = stdout.lines().any(|l| {
            l.trim()
                .strip_prefix("test result: ok. ")
                .and_then(|r| r.strip_suffix(" passed; 0 failed"))
                .and_then(|n| n.parse::<usize>().ok())
                .is_some_and(|n| n > 0)
        });
        assert!(
            out.status.success() && ran,
            "{rel} ({}): `velt test` {}\n{stdout}{}",
            if release { "release" } else { "debug" },
            if out.status.success() {
                "ran no tests"
            } else {
                "failed"
            },
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Copies directory `from` to `to` (made), recursively, leaving out build output (`target/`).
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "target" {
                copy_dir(&entry.path(), &dest);
            }
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}
