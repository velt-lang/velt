//! `velt native headers` (docs/internals/design/c-ffi.md "Checking declarations against C
//! headers"): check the current package's `declare c` functions against their libraries'
//! headers on every target that has C headers, and record what passed in `c-headers.json`
//! ([`vpm::c_headers`]), which builds that fetch the package compare their declarations with.
//! The values of its `declare c const`s on each target are recorded there too. `velt publish`
//! runs it first for a package with C libraries.

use std::path::Path;

use vpm::c_headers::CHeaders;

use super::project::Project;
use crate::driver::{BuildError, Session};
use crate::style;

/// The targets checked, one per set of C headers zig has (Windows: MSVC's sizes, read from
/// zig's MinGW headers).
const TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
    "aarch64-pc-windows-msvc",
    "x86_64-pc-windows-msvc",
];

/// `velt native headers`.
pub fn native_headers() -> Result<(), String> {
    let root = Project::current_root()?;
    let record = record(&root)?;
    if record.checked.is_empty() && record.constants.is_empty() {
        style::status(
            "Checked",
            "nothing: the package declares no C functions or constants to check",
        );
        return Ok(());
    }
    let libraries: usize = record.checked.values().map(|l| l.len()).max().unwrap_or(0);
    let constants: usize = record
        .constants
        .values()
        .map(|c| c.len())
        .max()
        .unwrap_or(0);
    let targets = record.checked.len().max(record.constants.len());
    let what = match (libraries, constants) {
        (l, 0) => format!("{l} C librar{}", if l == 1 { "y" } else { "ies" }),
        (0, c) => format!("{c} C constant{}", if c == 1 { "" } else { "s" }),
        (l, c) => format!(
            "{l} C librar{} and {c} constant{}",
            if l == 1 { "y" } else { "ies" },
            if c == 1 { "" } else { "s" }
        ),
    };
    style::status(
        "Checked",
        &format!(
            "{what} against its headers on {targets} targets; recorded in {}",
            vpm::c_headers::FILE
        ),
    );
    Ok(())
}

/// Check the package at `root` (the current directory's) on every target and write its
/// record; `Err` (with the diagnostics shown) when a check fails, and nothing is written. With
/// nothing to record there is no file: one left from before goes.
pub(super) fn record(root: &Path) -> Result<CHeaders, String> {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut record = CHeaders::default();
    for target in TARGETS {
        let mut sess = Session::new();
        let result = super::check::check_package_for(&mut sess, target);
        if !matches!(result, Ok(())) {
            super::build::report(&sess, false);
            return Err(match result {
                Err(BuildError::Failed(msg)) => msg,
                _ => format!("the package's check for {target} failed; nothing was recorded"),
            });
        }
        let Some(zig_target) = crate::native::zig_target(target) else {
            continue;
        };
        let mine = sess
            .c_checked
            .into_iter()
            .find(|(r, _)| r.canonicalize().unwrap_or_else(|_| r.clone()) == canonical);
        if let Some((_, libraries)) = mine {
            record.checked.insert(zig_target.to_string(), libraries);
        }
        let constants = sess
            .c_constants
            .into_iter()
            .find(|(r, _)| r.canonicalize().unwrap_or_else(|_| r.clone()) == canonical);
        if let Some((_, constants)) = constants {
            record.constants.insert(zig_target.to_string(), constants);
        }
    }
    if record.checked.is_empty() && record.constants.is_empty() {
        let _ = std::fs::remove_file(root.join(vpm::c_headers::FILE));
    } else {
        record.write(root)?;
    }
    Ok(record)
}
