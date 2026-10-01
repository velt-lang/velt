//! Shared helpers for the formatter's integration tests: the corpus of real `.vlt` files and a
//! span/NodeId-insensitive AST comparison, and the formatter's guarantees as one check.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use velt_common::FileId;
use velt_fmt::{count_comments, format_source};

/// Workspace root.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `.vlt` file under `tests/golden` and `std` (whatever exists), plus the directories
/// listed in `VELT_FMT_EXTRA_CORPUS` (separated like `PATH`), sorted.
pub fn corpus() -> Vec<PathBuf> {
    let mut out = vec![];
    for dir in ["tests/golden", "std"] {
        collect(&root().join(dir), &mut out);
    }
    if let Some(extra) = std::env::var_os("VELT_FMT_EXTRA_CORPUS") {
        for dir in std::env::split_paths(&extra) {
            collect(&dir, &mut out);
        }
    }
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "vlt") {
            out.push(path);
        }
    }
}

/// Does `src` parse without errors?
pub fn parses(src: &str) -> bool {
    let (_, diags) = velt_syntax::parse_file(FileId(0), src);
    !diags.iter().any(|d| d.is_error())
}

/// The AST dump of `src` with spans and node ids erased, so two sources that differ only in
/// layout compare equal.
pub fn ast_shape(src: &str) -> String {
    let (module, _) = velt_syntax::parse_file(FileId(0), src);
    let dump = velt_syntax::dump(&module);
    let mut out = String::with_capacity(dump.len());
    let mut skip_number = false;
    for line in dump.lines() {
        let t = line.trim();
        if skip_number {
            skip_number = false;
            if t.trim_end_matches(',').chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
        }
        let is_offset = (t.starts_with("lo: ") || t.starts_with("hi: "))
            && t[4..]
                .trim_end_matches(',')
                .chars()
                .all(|c| c.is_ascii_digit());
        if is_offset {
            continue;
        }
        if t.ends_with("NodeId(") {
            skip_number = true;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// First differing line of two texts, for readable assertion messages.
pub fn first_difference(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!("line {}:\n  - {x}\n  + {y}", i + 1);
        }
    }
    format!(
        "lengths differ: {} vs {} lines",
        a.lines().count(),
        b.lines().count()
    )
}

/// The formatter's guarantees for one parseable source.
pub fn check(src: &str) -> Result<(), String> {
    let once = format_source(src).map_err(|d| format!("refused: {d:?}"))?;
    if !parses(&once) {
        return Err(format!("output does not parse:\n{once}"));
    }
    let (before, after) = (ast_shape(src), ast_shape(&once));
    if before != after {
        return Err(format!(
            "AST changed: {}",
            first_difference(&before, &after)
        ));
    }
    if count_comments(src) != count_comments(&once) {
        return Err(format!("comments lost:\n{once}"));
    }
    let twice = format_source(&once).map_err(|d| format!("output refused: {d:?}"))?;
    if once != twice {
        return Err(format!(
            "not idempotent: {}",
            first_difference(&once, &twice)
        ));
    }
    Ok(())
}
