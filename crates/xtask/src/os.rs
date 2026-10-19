//! Whether a change needs the merge queue to check Windows and macOS too, or Linux is enough.
//!
//! The OS-specific code is in the runtime, the native crates, code generation and linking, the
//! `velt` command line and packages, and the standard library; a change there (or to the build,
//! CI or scripts, which select everything) runs every OS in the queue. The front end and the
//! tooling are portable Rust: Linux checks them, and the full gate on Windows and macOS after
//! the push to `main` catches the rare exception. A change the queue checked on every OS skips
//! that second run ([`after_merge_reason`]).

use crate::graph::Graph;

/// Crates without OS-specific code. A crate not listed here (a new one too) counts as
/// OS-specific.
pub(crate) const PORTABLE_CRATES: &[&str] = &[
    "velt_common",
    "velt_syntax",
    "velt_sema",
    "velt_vir",
    "velt_opt",
    "velt_fmt",
    "velt_lsp",
    "velt_doc",
    "velt_tscompat",
    "velt_registry",
    "velt_http",
    "velt_rt_wasm",
];

/// Paths outside `crates/` whose behavior can differ by OS: the standard library's bindings,
/// end-to-end programs and their expected output, native packages.
const OS_SPECIFIC_PATHS: &[&str] = &["std/", "tests/golden/", "examples/", "packages/"];

/// Why Windows and macOS must be checked too, or `None` when Linux is enough. `full` is
/// whether the paths select everything (build, CI and script changes).
pub fn other_os_reason(graph: &Graph, paths: &[String], full: bool) -> Option<String> {
    if full {
        return Some("the change selects every check".into());
    }
    for path in paths {
        if let Some(rest) = path.strip_prefix("crates/") {
            let dir = rest.split('/').next().unwrap_or(rest);
            match graph.dirs.get(dir) {
                Some(name) if PORTABLE_CRATES.contains(&name.as_str()) => {}
                Some(name) => return Some(format!("{name} has OS-specific code")),
                None => return Some(format!("{path}: unknown crate")),
            }
        } else if OS_SPECIFIC_PATHS.iter().any(|p| path.starts_with(p)) {
            return Some(format!("{path} can behave differently by OS"));
        }
    }
    None
}

/// Files that key or fill the build cache (Swatinem/rust-cache in `.github/actions/setup`): the
/// toolchain and the dependencies. Only the pushes to `main` save the cache.
const CACHE_INPUTS: &[&str] = &[
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/",
    ".github/actions/setup/",
];

/// Why the whole gate runs on Windows and macOS after a change lands on `main`, or `None` when
/// the merge queue already ran it there. `other_os` is the change's [`other_os_reason`]: on the
/// push, `cargo xtask affected --base HEAD^1` plans the same change the queue planned (the queue
/// tests each entry on its `base_sha`, which becomes `HEAD^1` on `main`). The gate runs when
///
/// - the queue checked Linux only, so Windows and macOS have not seen the change, or
/// - the change touches the toolchain or the dependencies: the run refreshes the Windows and
///   macOS build caches, which only pushes to `main` save.
pub fn after_merge_reason(paths: &[String], other_os: &Option<String>) -> Option<String> {
    if other_os.is_none() {
        return Some("the merge queue checked Linux only".into());
    }
    paths
        .iter()
        .find(|p| CACHE_INPUTS.iter().any(|c| p.starts_with(c)))
        .map(|p| format!("{p} changed: refresh the build cache"))
}
