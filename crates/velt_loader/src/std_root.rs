//! Locating the standard library sources and the implicitly loaded prelude.

use std::path::{Path, PathBuf};

use velt_common::std_modules;

/// The std root: `$VELT_STD`; else the installed layout (`<prefix>/bin/velt` → `<prefix>/std`);
/// else, walking up from the executable to the first directory with a `Cargo.toml` (dev layout:
/// `<repo>/target/<profile>/velt`), its `std/`; else `<exe dir>/std`. `None` if none of these
/// exists (last resort for dev builds: the compiling checkout's `std/`). Stopping at the first workspace keeps a git worktree nested in another checkout from
/// picking up the outer checkout's std.
///
/// The path is absolute, without `.` or `..` components, like the paths the loader gives std's
/// files: lowering tells std's files from the program's by this prefix (velt_vir's `LocMap`), and
/// a root of `crates/velt_loader/../../std` matched none of them, so on WebAssembly std's own C
/// calls were refused as a package's.
pub fn std_root() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VELT_STD").filter(|s| !s.is_empty()) {
        return Some(vpm::relpath::absolute(Path::new(&p)));
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
    let source_std =
        vpm::relpath::normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"));
    source_std.is_dir().then_some(source_std)
}

/// EXPERIMENT (#2943 M4): the std root when it is an installed release's own copy, which no
/// one edits: `$VELT_STD` is not set, the compiler runs from `<root>/toolchains/<version>/bin`
/// with std at `<root>/toolchains/<version>/std` (not a prefix linked under a name, which may
/// be a checkout's build), and that directory is read-only (the installer leaves a release's std
/// so). Only then may a check take std's files as the snapshot kept beside std's unit says,
/// without a `stat` of each (`velt_loader::std_tree`); any other std is checked file by file,
/// so an edit to it is never served a stale unit.
pub fn installed_std_root() -> Option<PathBuf> {
    if std::env::var_os("VELT_STD").is_some_and(|s| !s.is_empty()) {
        return None;
    }
    installed_std_for_exe(&std::env::current_exe().ok()?)
}

/// Testable core of [`installed_std_root`] (without `$VELT_STD`) for the executable at `exe`.
fn installed_std_for_exe(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    let prefix = bin.parent()?;
    let toolchains = prefix.parent()?;
    let layout = bin.file_name().is_some_and(|n| n == "bin")
        && toolchains.file_name().is_some_and(|n| n == "toolchains");
    let std = prefix.join("std");
    let read_only = std::fs::metadata(&std).is_ok_and(|m| m.is_dir() && m.permissions().readonly());
    (layout && read_only).then(|| vpm::relpath::absolute(&std))
}

/// Whether `file` is inside the std root `std_root` (both resolved like the loader's file keys).
pub fn in_std(file: &Path, std_root: &Path) -> bool {
    let key = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| vpm::relpath::absolute(p));
    key(file).starts_with(key(std_root))
}

/// The prelude's files in load order, as `std/prelude/index.velt` lists them
/// ([`velt_common::std_modules::prelude_order`], docs/internals/design/std-layout.md §4). No
/// prelude file is no prelude; prelude files without a readable, valid index are an error.
pub fn prelude_files(std_root: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = std_root.join("prelude");
    let index = std_root.join(std_modules::PRELUDE_INDEX);
    let has_files = || {
        vpm::probe::read_dir(&dir).is_ok_and(|entries| {
            entries.iter().any(|e| {
                let p = e.path();
                vpm::probe::is_file(&p) && p.extension().is_some_and(|x| x == "velt")
            })
        })
    };
    if !vpm::probe::is_file(&index) && !has_files() {
        return Ok(vec![]);
    }
    let src = vpm::probe::read_to_string(&index).map_err(|e| {
        format!(
            "cannot read the prelude's load order `{}`: {e}",
            index.display()
        )
    })?;
    let order =
        std_modules::prelude_order(&src).map_err(|e| format!("`{}`, {e}", index.display()))?;
    Ok(order
        .into_iter()
        .map(|name| dir.join(format!("{name}.velt")))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_prelude_in_index_order() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(prelude_files(tmp.path()).unwrap().is_empty());
        let dir = tmp.path().join("prelude");
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["b.velt", "a.velt", "notes.md"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let err = prelude_files(tmp.path()).unwrap_err();
        assert!(
            err.contains("cannot read the prelude's load order"),
            "{err}"
        );
        // The index decides the order, not the file names.
        std::fs::write(
            dir.join("index.velt"),
            "// Prelude.\nimport \"./b\";\nimport \"./a\";\n",
        )
        .unwrap();
        assert_eq!(
            prelude_files(tmp.path()).unwrap(),
            [dir.join("b.velt"), dir.join("a.velt")]
        );
        std::fs::write(dir.join("index.velt"), "export { x } from \"./a\";\n").unwrap();
        let err = prelude_files(tmp.path()).unwrap_err();
        assert!(err.contains("line 1: expected `import"), "{err}");
    }

    /// Make directory `dir` read-only (`on`) or writable by its owner again.
    fn read_only_dir(dir: &Path, on: bool) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if on { 0o555 } else { 0o755 };
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut p = std::fs::metadata(dir).unwrap().permissions();
            p.set_readonly(on);
            std::fs::set_permissions(dir, p).unwrap();
        }
    }

    /// Only a read-only std of a release under `<root>/toolchains/<version>` is taken without a
    /// per-file check; a writable one, a linked prefix's or a dev build's is checked file by
    /// file (`installed_std_root`; `$VELT_STD` is the caller's, checked before).
    #[test]
    fn only_a_read_only_release_std_is_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let release = tmp.path().join("toolchains/0.4.0");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        let std = release.join("std");
        std::fs::create_dir_all(&std).unwrap();
        let exe = release.join("bin/velt");

        // A writable release std: checked file by file.
        assert_eq!(installed_std_for_exe(&exe), None);

        read_only_dir(&std, true);
        assert_eq!(
            installed_std_for_exe(&exe),
            Some(vpm::relpath::absolute(&std))
        );

        // The same read-only std in a prefix linked under a name, not a release's.
        let linked = tmp.path().join("links-target/dev");
        std::fs::create_dir_all(linked.join("bin")).unwrap();
        std::fs::create_dir_all(linked.join("std")).unwrap();
        read_only_dir(&linked.join("std"), true);
        assert_eq!(installed_std_for_exe(&linked.join("bin/velt")), None);

        // A dev build's std beside the checkout's Cargo.toml.
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("target/debug")).unwrap();
        std::fs::create_dir_all(repo.join("std")).unwrap();
        assert_eq!(installed_std_for_exe(&repo.join("target/debug/velt")), None);

        // Writable again (someone edits the release's std): checked file by file again.
        read_only_dir(&std, false);
        assert_eq!(installed_std_for_exe(&exe), None);

        // tempdir removes the tree: leave every directory writable.
        read_only_dir(&linked.join("std"), false);
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

        // A target dir outside any checkout: the std this crate was compiled from, without the
        // `..` that kept its files from matching the loader's paths.
        let outside = root.join("outside/debug");
        std::fs::create_dir_all(&outside).unwrap();
        let std = std_root_for_exe(&outside.join("velt")).expect("the source tree's std");
        assert!(
            std.components().all(|c| !matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )),
            "{}",
            std.display()
        );
        assert!(std.join("prelude").is_dir(), "{}", std.display());
    }
}
