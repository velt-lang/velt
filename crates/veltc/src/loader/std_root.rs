//! Locating the standard library sources and the implicitly loaded prelude.

use std::path::{Path, PathBuf};

/// The std root: `$VELT_STD`; else the installed layout (`<prefix>/bin/velt` → `<prefix>/std`);
/// else, walking up from the executable to the first directory with a `Cargo.toml` (dev layout:
/// `<repo>/target/<profile>/velt`), its `std/`; else `<exe dir>/std`. `None` if none of these
/// exists (last resort for dev builds: the compiling checkout's `std/`). Stopping at the first workspace keeps a git worktree nested in another checkout from
/// picking up the outer checkout's std.
pub fn std_root() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VELT_STD").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(p));
    }
    std_root_for_exe(&std::env::current_exe().ok()?)
}

/// Testable core of [`std_root`] (without `$VELT_STD`) for the executable at `exe`.
fn std_root_for_exe(exe: &Path) -> Option<PathBuf> {
    let exe_dir = exe.parent()?;
    // Only a `bin/` directory marks the installed layout, so a dev build never picks up an
    // unrelated `std/` next to `target/`.
    if exe_dir.file_name().is_some_and(|n| n == "bin") {
        let installed = exe_dir.parent().map(|prefix| prefix.join("std"));
        if let Some(std) = installed.filter(|d| d.is_dir()) {
            return Some(std);
        }
    }
    if let Some(repo) = exe_dir.ancestors().find(|d| d.join("Cargo.toml").is_file()) {
        let std = repo.join("std");
        if std.is_dir() {
            return Some(std);
        }
    }
    let beside = exe_dir.join("std");
    if beside.is_dir() {
        return Some(beside);
    }
    // Dev builds whose target dir lives outside the checkout (`CARGO_TARGET_DIR` elsewhere):
    // fall back to the std of the source tree this binary was compiled from.
    let source_std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    source_std.is_dir().then_some(source_std)
}

/// `std/prelude/*.vlt`, sorted by file name (empty if the directory is missing).
pub fn prelude_files(std_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(std_root.join("prelude")) else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "vlt"))
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_prelude_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(prelude_files(tmp.path()).is_empty());
        let dir = tmp.path().join("prelude");
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["b.vlt", "a.vlt", "notes.md"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        assert_eq!(
            prelude_files(tmp.path()),
            [dir.join("a.vlt"), dir.join("b.vlt")]
        );
    }

    #[test]
    fn finds_installed_and_dev_layouts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Installed: <prefix>/bin/velt + <prefix>/std.
        let prefix = root.join("prefix");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(prefix.join("std")).unwrap();
        assert_eq!(
            std_root_for_exe(&prefix.join("bin/velt")),
            Some(prefix.join("std"))
        );

        // Dev: <repo>/target/debug/velt + <repo>/Cargo.toml + <repo>/std.
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("target/debug")).unwrap();
        std::fs::create_dir_all(repo.join("std")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "").unwrap();
        assert_eq!(
            std_root_for_exe(&repo.join("target/debug/velt")),
            Some(repo.join("std"))
        );

        // Portable: std next to the executable.
        let flat = root.join("flat");
        std::fs::create_dir_all(flat.join("std")).unwrap();
        assert_eq!(std_root_for_exe(&flat.join("velt")), Some(flat.join("std")));
    }
}
