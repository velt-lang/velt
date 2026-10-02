//! Whether a change needs the merge queue to check Windows and macOS too, or Linux is enough.
//!
//! The OS-specific code is in the runtime, the native crates, code generation and linking, the
//! `velt` command line and packages, and the standard library; a change there (or to the build,
//! CI or scripts, which select everything) runs every OS in the queue. The front end and the
//! tooling are portable Rust: Linux checks them, and the full gate on Windows and macOS after
//! each push to `main` catches the rare exception.

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
