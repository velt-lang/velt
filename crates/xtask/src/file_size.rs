//! The file-size rule (CONTRIBUTING.md "Coding standards"): Rust sources under `crates/` stay
//! under 800 lines, test files (a path with a `tests` directory) under 1000.
//!
//! One definition for two users: `cargo xtask precheck` checks the changed files with it, and
//! crates/veltc/tests/standards.rs (which includes this file with `#[path]`) checks every file.

/// The hard limit for source files.
pub const SRC_LIMIT: usize = 800;
/// The hard limit for test files.
pub const TEST_LIMIT: usize = 1000;

/// Files temporarily over the limit, with the milestone that must split them.
pub const ALLOWED_OVERSIZE: &[(&str, &str)] = &[];

/// Whether the rule covers `rel` (relative to the repository root, `/`-separated).
pub fn applies(rel: &str) -> bool {
    rel.starts_with("crates/")
        && rel.ends_with(".rs")
        && !rel.split('/').any(|part| part == "target")
        && !ALLOWED_OVERSIZE.iter().any(|(path, _)| *path == rel)
}

/// The limit for `rel`.
pub fn limit(rel: &str) -> usize {
    if rel.contains("/tests/") {
        TEST_LIMIT
    } else {
        SRC_LIMIT
    }
}

/// The violation message for `rel` with contents `text`, if it is over its limit.
pub fn violation(rel: &str, text: &str) -> Option<String> {
    let limit = limit(rel);
    let lines = text.lines().count();
    (lines > limit).then(|| format!("{rel}: {lines} lines (limit {limit}) — split it by concern"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_get_the_larger_limit() {
        let lines = |n: usize| "x\n".repeat(n);
        assert_eq!(violation("crates/a/src/lib.rs", &lines(800)), None);
        assert_eq!(
            violation("crates/a/src/lib.rs", &lines(801)).as_deref(),
            Some("crates/a/src/lib.rs: 801 lines (limit 800) — split it by concern")
        );
        assert_eq!(violation("crates/a/tests/big.rs", &lines(1000)), None);
        assert!(violation("crates/a/tests/big.rs", &lines(1001)).is_some());
        assert!(violation("crates/a/src/x/tests/t.rs", &lines(900)).is_none());
    }

    #[test]
    fn only_rust_sources_under_crates_count() {
        assert!(applies("crates/a/src/lib.rs"));
        assert!(applies("crates/a/tests/t.rs"));
        assert!(!applies("crates/a/README.md"));
        assert!(!applies("tests/difftest/src/main.rs"));
        assert!(!applies("crates/a/target/debug/build/out.rs"));
    }
}
