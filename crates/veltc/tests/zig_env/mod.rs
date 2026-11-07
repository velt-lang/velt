//! The zig 0.17.1 that the tests checking C headers and building C sources run (`VELT_ZIG`).

use std::ffi::OsStr;
use std::path::PathBuf;

/// The zig named by `VELT_ZIG`, or `None` (and a note that the test skipped, so it can't `does`)
/// when it names none. With `VELT_ZIG_TESTS` set (the `c-headers` job, which runs these files:
/// crates/xtask/src/c_headers.rs) that fails instead, so a job that loses its zig goes red
/// rather than passing with nothing checked. Other jobs may have `VELT_ZIG` to build C sources,
/// and leave these files out.
pub fn zig(does: &str) -> Option<PathBuf> {
    let zig = std::env::var_os("VELT_ZIG");
    let on = std::env::var_os("VELT_ZIG_TESTS");
    match choose(zig.as_deref(), on.as_deref()) {
        Choice::Run(zig) => Some(zig),
        Choice::Fail => panic!(
            "VELT_ZIG_TESTS is set but VELT_ZIG isn't: these checks need zig 0.17.1 \
             (.github/actions/zig)"
        ),
        Choice::Skip => {
            eprintln!("skipped: set VELT_ZIG to a zig 0.17.1 to {does}");
            None
        }
    }
}

#[derive(Debug, PartialEq)]
enum Choice {
    Run(PathBuf),
    Fail,
    Skip,
}

/// What to do with `VELT_ZIG` and `VELT_ZIG_TESTS`: the latter is on when set to anything but
/// empty or `0`, as crates/xtask/src/c_headers.rs reads it.
fn choose(zig: Option<&OsStr>, on: Option<&OsStr>) -> Choice {
    let on = on.is_some_and(|v| !v.is_empty() && v != "0");
    match zig {
        Some(z) if !z.is_empty() => Choice::Run(PathBuf::from(z)),
        _ if on => Choice::Fail,
        _ => Choice::Skip,
    }
}

#[test]
fn zig_env_fails_only_when_the_zig_tests_are_on_without_zig() {
    let s = |v: &'static str| Some(OsStr::new(v));
    assert_eq!(choose(s("/z/zig"), None), Choice::Run("/z/zig".into()));
    assert_eq!(choose(s("/z/zig"), s("1")), Choice::Run("/z/zig".into()));
    assert_eq!(choose(None, s("1")), Choice::Fail);
    assert_eq!(choose(s(""), s("1")), Choice::Fail);
    // `CI` alone no longer fails: jobs that build C sources have zig without these tests.
    assert_eq!(choose(None, None), Choice::Skip);
    assert_eq!(choose(None, s("0")), Choice::Skip);
    assert_eq!(choose(None, s("")), Choice::Skip);
}
