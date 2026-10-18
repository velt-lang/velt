//! Shared by the launcher's tests.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The stand-in toolchain binary (fake_velt.rs here), compiled with `rustc` once for each
/// version of its source. (Not an example: building one from a test would rebuild the package's
/// dependencies with other features, and a filtered `cargo test` does not build examples.)
///
/// Once in place it is never replaced, so every test process reads the same bytes. nextest runs
/// each test in a process of its own, and several may find the binary missing and build it at
/// once; on Windows no two builds are byte-identical (the PE header's time stamp and the PDB
/// id), so a process that replaced it could change it under another between building a release
/// archive from it and comparing what got installed (#1053). Hence the directory named after
/// the source's hash (an edited source gets a binary of its own), and the hard link that puts a
/// build in place only if none is there yet: the first one wins, the others are discarded.
pub fn fake_velt() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_velt.rs");
            let hash = velt_toolchain::install::sha256_hex(&std::fs::read(&source).unwrap());
            let dir = Path::new(env!("CARGO_BIN_EXE_velt-launcher"))
                .parent()
                .unwrap()
                .join("fake-velt")
                .join(&hash[..16]);
            let exe = dir.join(format!("fake_velt{}", std::env::consts::EXE_SUFFIX));
            if !exe.is_file() {
                // Built in a directory of this process's own: test processes running at once
                // never see half a file, and never share rustc's intermediate objects (named
                // after the crate, next to the output), which one could truncate while another
                // links them.
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
                match std::fs::hard_link(&partial, &exe) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => panic!("putting {} in place: {e}", exe.display()),
                }
                let _ = std::fs::remove_dir_all(&work);
            }
            exe
        })
        .clone()
}
