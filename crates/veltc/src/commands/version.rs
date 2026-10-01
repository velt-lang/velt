//! `velt --version`: the package version plus the git commit it was built from (embedded by
//! `build.rs`, absent outside a git checkout) and the host triple.

/// `velt <version> (<git hash> <host triple>)`, or `velt <version> (<host triple>)` without a hash.
pub fn version_line() -> String {
    format_version(
        env!("CARGO_PKG_VERSION"),
        env!("VELT_GIT_HASH"),
        &velt_codegen_cl::host_triple(),
    )
}

fn format_version(version: &str, hash: &str, host: &str) -> String {
    if hash.is_empty() {
        format!("velt {version} ({host})")
    } else {
        format!("velt {version} ({hash} {host})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_with_and_without_hash() {
        assert_eq!(
            format_version("0.1.0", "abc1234", "x86_64-pc-windows-msvc"),
            "velt 0.1.0 (abc1234 x86_64-pc-windows-msvc)"
        );
        assert_eq!(
            format_version("0.1.0", "", "aarch64-apple-darwin"),
            "velt 0.1.0 (aarch64-apple-darwin)"
        );
        assert!(version_line().starts_with(&format!("velt {} (", env!("CARGO_PKG_VERSION"))));
    }
}
