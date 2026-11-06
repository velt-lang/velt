//! What the check of `declare c` against C headers depends on ([`crate::plan::Plan::c_headers`]):
//! a change to these runs it through zig, on one Linux job. Anything else doesn't, not even a
//! change that selects everything (a lockfile, CI, a script), so that no other pull request or
//! queue entry downloads zig. The nightly runs it whatever changed, which catches what this list
//! misses.

/// The veltc test files that check through zig (`crates/veltc/tests/<name>.rs`). A change to one
/// runs the `c-headers` job, which runs them; every other test run leaves them out unless
/// `VELT_ZIG` is set ([`without_zig_tests`]), since in CI they fail without zig rather than skip.
/// The job's `cargo test` in `.github/workflows/ci.yml` and `nightly.yml` names the same files.
pub const TESTS: &[&str] = &["c_headers"];

const PATHS: &[&str] = &[
    "crates/veltc/src/native/",
    "crates/veltc/src/commands/native_headers.rs",
    "crates/velt_toolchain/src/zig.rs",
    "crates/velt_toolchain/examples/zig.rs",
    "crates/vpm/src/c_headers.rs",
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

/// `filterset` (nextest's) without the [`TESTS`]: what a test run without `VELT_ZIG` runs.
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
    fn a_test_run_without_zig_leaves_out_the_tests_that_need_it() {
        assert_eq!(
            without_zig_tests("all() - binary_id(veltc::golden)"),
            "(all() - binary_id(veltc::golden)) - (binary_id(veltc::c_headers))"
        );
    }
}
