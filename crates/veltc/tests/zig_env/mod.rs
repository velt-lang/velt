//! The zig 0.17.1 that the tests checking C headers and building C sources run (`VELT_ZIG`).

use std::path::PathBuf;

/// The zig named by `VELT_ZIG`, or `None` (and a note that the test skipped, so it can't `does`)
/// when it names none. In CI that fails instead, so a job that loses its zig goes red rather than
/// passing with nothing checked (the other test runs leave these files out:
/// crates/xtask/src/c_headers.rs).
pub fn zig(does: &str) -> Option<PathBuf> {
    match std::env::var_os("VELT_ZIG") {
        Some(z) => Some(PathBuf::from(z)),
        None if std::env::var_os("CI").is_some() => {
            panic!(
                "CI is set but VELT_ZIG isn't: these checks need zig 0.17.1 (.github/actions/zig)"
            )
        }
        None => {
            eprintln!("skipped: set VELT_ZIG to a zig 0.17.1 to {does}");
            None
        }
    }
}
