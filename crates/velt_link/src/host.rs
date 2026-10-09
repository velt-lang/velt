//! The host target: the platform this `velt` runs on. Its runtime libraries sit in the
//! toolchain's `lib/` (and in a checkout's `target/<profile>/`); another target's live in its
//! own directory, `lib/targets/<triple>/`, beside its link kit (a target pack, #856), so a build
//! for another target never picks up the host's runtime of the same file name.

/// The target triple of the platform this `velt` runs on (`x86_64-pc-windows-msvc`,
/// `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, `…-linux-musl`). The same spelling as
/// `velt_codegen_cl::host_triple`, which `velt` uses for builds without `--target`.
pub fn host_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "windows" => format!("{arch}-pc-windows-msvc"),
        "macos" => format!("{arch}-apple-darwin"),
        _ if cfg!(target_env = "musl") => format!("{arch}-unknown-linux-musl"),
        _ => format!("{arch}-unknown-linux-gnu"),
    }
}

/// Whether two triples name the same platform: architecture (`arm64` = `aarch64`), operating
/// system and C library, whatever the vendor field and OS spelling (`apple-darwin`,
/// `apple-macosx11.0`).
pub fn same_target(a: &str, b: &str) -> bool {
    key(a) == key(b)
}

/// (architecture, OS family, musl).
fn key(triple: &str) -> (String, Option<crate::TargetOs>, bool) {
    let arch = match triple.split('-').next().unwrap_or_default() {
        "arm64" => "aarch64",
        other => other,
    };
    (
        arch.to_string(),
        crate::TargetOs::from_triple(triple),
        triple.contains("musl"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_is_a_known_target() {
        let host = host_triple();
        assert!(crate::TargetOs::from_triple(&host).is_some(), "{host}");
        assert!(same_target(&host, &host));
    }

    #[test]
    fn targets_compare_by_arch_os_and_libc() {
        assert!(same_target(
            "aarch64-apple-darwin",
            "arm64-apple-macosx11.0"
        ));
        assert!(same_target("x86_64-unknown-linux-gnu", "x86_64-linux-gnu"));
        assert!(!same_target(
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu"
        ));
        assert!(!same_target(
            "x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl"
        ));
        assert!(!same_target(
            "x86_64-pc-windows-msvc",
            "x86_64-apple-darwin"
        ));
    }
}
