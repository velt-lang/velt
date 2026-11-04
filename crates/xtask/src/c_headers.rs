//! What the check of `declare c` against C headers depends on ([`crate::plan::Plan::c_headers`]):
//! a change to these runs it through zig, on one Linux job. Anything else doesn't, not even a
//! change that selects everything (a lockfile, CI, a script), so that no other pull request or
//! queue entry downloads zig. The nightly runs it whatever changed, which catches what this list
//! misses.

const PATHS: &[&str] = &[
    "crates/veltc/src/native/headers/",
    "crates/veltc/src/native/c_decls.rs",
    "crates/veltc/src/commands/native_headers.rs",
    "crates/veltc/tests/c_headers.rs",
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
    PATHS
        .iter()
        .any(|p| path == *p || (p.ends_with('/') && path.starts_with(p)))
}
