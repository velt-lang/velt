//! What the constant-time check of std/crypto depends on ([`crate::plan::Plan::ct_counts`]):
//! a change to these runs crates/veltc/tests/ct_counts.rs under valgrind, on one Linux job
//! (`ct-counts` in .github/workflows/ci.yml), with `VELT_CT_TESTS` set so that a job without
//! valgrind fails instead of skipping. Elsewhere the test measures only where valgrind happens
//! to be installed, which no other CI job has (#3942: in CI it passed without measuring
//! anything). The nightly runs it whatever changed: a change to the optimizer or the code
//! generator can turn a mask into a branch too (#3368), and this list doesn't name them, so that
//! every such change doesn't pay for the job.

const PATHS: &[&str] = &[
    // The secret-dependent code, its probes and the check.
    "std/crypto/",
    "bench/crypto/ct/",
    "scripts/ct-counts.py",
    "scripts/ct-shim.c",
    "crates/veltc/tests/ct_counts.rs",
    "crates/veltc/tests/ct_counts/",
    // `@noInline`, which keeps a mask out of its callers (#3368), and the release build's
    // objects of the functions it marks (std_cache/no_inline.rs).
    "std/lang/intrinsics/decorators.velt",
    "crates/veltc/src/driver/std_cache/no_inline.rs",
    "crates/xtask/src/ct_counts.rs",
];

/// Whether the check depends on `path`.
pub fn depends_on(path: &str) -> bool {
    PATHS
        .iter()
        .any(|p| path == *p || (p.ends_with('/') && path.starts_with(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crypto_and_the_check_run_it() {
        assert!(depends_on("std/crypto/ed25519_sign.velt"));
        assert!(depends_on("bench/crypto/ct/ed25519.probe"));
        assert!(depends_on("scripts/ct-counts.py"));
        assert!(depends_on("crates/veltc/tests/ct_counts.rs"));
        assert!(!depends_on("std/cryptography.velt"));
        assert!(!depends_on("crates/veltc/tests/ct_counts.rs.orig"));
        assert!(!depends_on("std/fs/index.velt"));
    }
}
