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
pub const TESTS: &[&str] = &[
    "c_headers",
    "c_constants",
    "c_sources",
    "c_export",
    "c_struct_pointers",
    "c_struct_headers",
    "c_struct_by_value",
];

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
    // std's own `declare c("c")` functions and the variants that load them per target
    // (`velt native headers --std`, tests/c_headers.rs).
    "std/fs/fd/",
    "std/fs/tree/",
    "std/fs/whole/",
    "std/imports.velt",
    // The other files crates/veltc/src/native/headers/std_check.rs checks (`CHECKED`,
    // `CHECKED_POSIX`): a change to one of them runs the check too.
    "std/lang/hooks/clock_darwin.velt",
    "std/lang/hooks/clock_linux.velt",
    "std/lang/hooks/clock_win32.velt",
    "std/lang/hooks/os_posix.velt",
    "std/lang/hooks/sched_cells.velt",
    "std/lang/hooks/sched_end.velt",
    "std/lang/hooks/sched_exit.velt",
    "std/lang/hooks/sched_id.velt",
    "std/lang/hooks/sched_io_write.velt",
    "std/lang/hooks/sched_mem.velt",
    "std/lang/hooks/sched_node_loc.velt",
    "std/lang/hooks/sched_off.velt",
    "std/lang/hooks/sched_peek.velt",
    "std/lang/hooks/sched_read.velt",
    "std/lang/hooks/sched_wake.velt",
    "std/lang/hooks/sched_workers.velt",
    "std/lang/hooks/thread_posix.velt",
    "std/lang/hooks/unwind_itanium.velt",
    "std/net/calls_posix.velt",
    "std/net/fd_io.velt",
    "std/net/fd_until.velt",
    "std/process/child/",
    "std/process/stdin_buffer.velt",
    "std/process/stdin_posix.velt",
    "std/process/stdin_win32.velt",
    "std/sync/output.velt",
    "std/sync/slot.velt",
    "std/time/localtime_posix.velt",
    "std/time/localtime_win32.velt",
    // The runtime's exports, which the check skips (std_check.rs `runtime_exports`).
    "crates/velt_common/tests/runtime_exports.txt",
    "crates/veltc/src/native/headers/std_check.rs",
    ".github/actions/zig/",
    // C variables (`declare c("lib") let`, tests/c_sources.rs): their lowering, the backends'
    // data imports, the dev JIT's lookup, the optimizer's and the std cache's handling of them.
    "crates/velt_vir/src/lower/c_global.rs",
    // C structs moved to and from C memory (`Ptr.read`, `Ptr.write`, tests/c_struct_pointers.rs):
    // their layout and the checks of what C wrote.
    "crates/velt_vir/src/lower/c_layout.rs",
    "crates/velt_vir/src/lower/ptr_memory.rs",
    "crates/velt_codegen_llvm/src/statics.rs",
    "crates/velt_codegen_cl/src/module.rs",
    "crates/velt_codegen_cl/src/c_libraries.rs",
    "crates/velt_codegen_cl/src/win_symbols.rs",
    "crates/velt_opt/src/interp/mod.rs",
    "crates/veltc/src/driver/std_cache/statics.rs",
    // C structs (#2187): the header check of structs (tests/c_struct_headers.rs, under
    // crates/veltc/src/native/) and structs by value, whose differential test is the
    // classifier's entry criterion (tests/c_struct_by_value.rs; ci.yml's c-struct-abi job runs
    // it on arm64 Linux with this job, and scripts/abi-cross.sh under qemu).
    "crates/velt_vir/src/c_struct_abi/",
    "scripts/abi-cross.sh",
    "crates/velt_vir/src/lower/c_struct_call.rs",
    "crates/velt_vir/src/lower/c_call.rs",
    "crates/velt_codegen_llvm/src/c_struct.rs",
    "crates/velt_codegen_llvm/src/function/terminator.rs",
    "crates/velt_codegen_cl/src/abi.rs",
    "crates/velt_codegen_cl/src/function/terminator.rs",
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
    fn c_variable_lowering_runs_the_check() {
        assert!(depends_on("crates/velt_vir/src/lower/c_global.rs"));
        assert!(depends_on("crates/velt_codegen_llvm/src/statics.rs"));
        assert!(depends_on("crates/velt_codegen_cl/src/c_libraries.rs"));
        assert!(!depends_on("crates/velt_vir/src/lower/expr.rs"));
    }

    #[test]
    fn a_test_run_without_zig_leaves_out_the_tests_that_need_it() {
        assert_eq!(
            without_zig_tests("all() - binary_id(veltc::golden)"),
            "(all() - binary_id(veltc::golden)) - (binary_id(veltc::c_headers) \
             | binary_id(veltc::c_constants) | binary_id(veltc::c_sources) \
             | binary_id(veltc::c_export) | binary_id(veltc::c_struct_pointers) \
             | binary_id(veltc::c_struct_headers) | binary_id(veltc::c_struct_by_value))"
        );
    }
}
