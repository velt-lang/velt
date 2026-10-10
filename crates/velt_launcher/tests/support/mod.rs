//! Shared by the launcher's tests.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The stand-in toolchain binary (fake_velt.rs here): compiled with `rustc` once per test
/// process, and again only when its source is newer. (Not an example: building one from a test
/// would rebuild the package's dependencies with other features, and a filtered `cargo test`
/// does not build examples.)
pub fn fake_velt() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_velt.rs");
            let dir = Path::new(env!("CARGO_BIN_EXE_velt-launcher"))
                .parent()
                .unwrap()
                .join("fake-velt");
            let exe = dir.join(format!("fake_velt{}", std::env::consts::EXE_SUFFIX));
            let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            if modified(&exe) < modified(&source) || modified(&exe).is_none() {
                // Built in a directory of this process's own and renamed: test processes running
                // at once never see half a file, and never share rustc's intermediate objects
                // (named after the crate, next to the output), which one could truncate while
                // another links them.
                let work = dir.join(format!("build.{}", std::process::id()));
                std::fs::create_dir_all(&work).unwrap();
                let partial = work.join(format!("fake_velt{}", std::env::consts::EXE_SUFFIX));
                let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
                let status = Command::new(rustc)
                    .args(["--edition", "2021", "-O", "-o"])
                    .arg(&partial)
                    .arg(&source)
                    .status()
                    .unwrap();
                assert!(status.success(), "compiling {} failed", source.display());
                std::fs::rename(&partial, &exe).unwrap();
                let _ = std::fs::remove_dir_all(&work);
            }
            exe
        })
        .clone()
}
