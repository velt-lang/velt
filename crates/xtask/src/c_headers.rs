//! What the check of `declare c` against C headers depends on ([`crate::plan::Plan::c_headers`]):
//! a change to these runs it through zig, on one Linux job. Anything else doesn't, not even a
//! change that selects everything (a lockfile, CI, a script), so that no other pull request or
//! queue entry runs it. (Jobs whose tests build a package's C `sources` get zig for that alone:
//! crates/xtask/src/zig.rs.) The nightly runs it whatever changed, which catches what this list
//! misses.

/// The veltc test files that check through zig (`crates/veltc/tests/<name>.rs`). A change to one
/// runs the `c-headers` job, which runs them with `VELT_ZIG_TESTS=1`; every other test run leaves
/// them out ([`without_zig_tests`]), also the jobs that have zig to build C sources (`VELT_ZIG`,
/// crates/xtask/src/zig.rs). With `VELT_ZIG_TESTS` set they fail without zig rather than skip
/// (crates/veltc/tests/zig_env). The job's `cargo test` in `.github/workflows/ci.yml` and
/// `nightly.yml` names the same files.
pub const TESTS: &[&str] = &["c_headers", "c_constants", "c_sources"];

const PATHS: &[&str] = &[
    "crates/veltc/src/native/",
    "crates/veltc/src/commands/native_headers.rs",
    // The zig the TESTS run, and their guard that fails in CI without it.
    "crates/veltc/tests/zig_env/",
    "crates/velt_toolchain/src/zig.rs",
    "crates/velt_toolchain/examples/zig.rs",
    "crates/vpm/src/c_headers.rs",
    // A package's C `sources`: built with zig, and found at install (tests/c_sources.rs).
    "crates/vpm/src/native/c_build.rs",
    "crates/vpm/src/install/c_sources.rs",
    // Which package may declare C functions, and from which source (#2379): the fetched-package
    // tests here depend on it.
    "crates/vpm/src/install/allow_native.rs",
    "crates/vpm/src/manifest/allow_native.rs",
    "crates/vpm/src/manifest/c_library.rs",
    "crates/vpm/src/manifest/read/native.rs",
    "std/ffi/",
    ".github/actions/zig/",
];

/// Whether the check depends on `path`.
pub fn depends_on(path: &str) -> bool {
    let test = path
        .strip_prefix("crates/veltc/tests/")
        .and_then(|f| f.strip_suffix(".rs"))
        .is_some_and(|name| TESTS.contains(&name));
    test || PATHS
        .iter()
        .any(|p| path == *p || (p.ends_with('/') && path.starts_with(p)))
}

/// The variable that turns the [`TESTS`] on, set only in the `c-headers` job.
pub const TESTS_VAR: &str = "VELT_ZIG_TESTS";

/// Whether this run includes the [`TESTS`]: [`TESTS_VAR`] is set to anything but empty or `0`,
/// the rule crates/veltc/tests/zig_env follows too.
pub fn zig_tests_on() -> bool {
    on(std::env::var_os(TESTS_VAR).as_deref())
}

fn on(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// `filterset` (nextest's) without the [`TESTS`]: what a test run without [`TESTS_VAR`] runs.
pub fn without_zig_tests(filterset: &str) -> String {
    let tests: Vec<String> = TESTS
        .iter()
        .map(|t| format!("binary_id(veltc::{t})"))
        .collect();
    format!("({filterset}) - ({})", tests.join(" | "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_set_variable_turns_the_tests_on() {
        use std::ffi::OsStr;
        assert!(on(Some(OsStr::new("1"))));
        assert!(on(Some(OsStr::new("yes"))));
        assert!(!on(Some(OsStr::new("0"))));
        assert!(!on(Some(OsStr::new(""))));
        assert!(!on(None));
    }

    #[test]
    fn a_test_run_without_zig_leaves_out_the_tests_that_need_it() {
        assert_eq!(
            without_zig_tests("all() - binary_id(veltc::golden)"),
            "(all() - binary_id(veltc::golden)) - (binary_id(veltc::c_headers) \
             | binary_id(veltc::c_constants) | binary_id(veltc::c_sources))"
        );
    }
}
