//! The native runtime staticlib must sit next to the `velt` binary before a test can link a
//! program. `cargo build -p velt_rt` makes sure it does when a test runs on its own.
//!
//! The gate (`cargo xtask check`) builds the whole workspace first and sets
//! `VELT_RT_PREBUILT=1`, so the tests skip that build: `-p velt_rt` resolves the runtime's
//! dependencies with other features than `--workspace` does, so it would compile the runtime
//! and part of its dependencies a second time, and a parallel test runner would wait on the
//! build lock in every test process.

use std::path::Path;
use std::process::Command;

pub fn build_native_runtime(root: &Path) {
    if std::env::var_os("VELT_RT_PREBUILT").is_some_and(|v| v == "1") {
        return;
    }
    let st = Command::new(env!("CARGO"))
        .args(["build", "-p", "velt_rt"])
        .current_dir(root)
        .status()
        .expect("cargo build -p velt_rt");
    assert!(st.success(), "building velt_rt failed");
}
