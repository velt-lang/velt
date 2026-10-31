//! Locating the standard library sources and the implicitly loaded prelude.

use std::path::{Path, PathBuf};

use velt_common::std_modules;

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

/// Whether `file` is inside the std root `std_root` (both resolved like the loader's file keys).
pub fn in_std(file: &Path, std_root: &Path) -> bool {
    let key = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| vpm::relpath::absolute(p));
    key(file).starts_with(key(std_root))
}

/// The prelude's files in load order, as `std/prelude/index.vlt` lists them
/// ([`velt_common::std_modules::prelude_order`], docs/internals/design/std-layout.md §4). No
/// prelude file is no prelude; prelude files without a readable, valid index are an error.
pub fn prelude_files(std_root: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = std_root.join("prelude");
    let index = std_root.join(std_modules::PRELUDE_INDEX);
    let has_files = || {
        vpm::probe::read_dir(&dir).is_ok_and(|entries| {
            entries.iter().any(|e| {
                let p = e.path();
                vpm::probe::is_file(&p) && p.extension().is_some_and(|x| x == "vlt")
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
        .map(|name| dir.join(format!("{name}.vlt")))
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
        for f in ["b.vlt", "a.vlt", "notes.md"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let err = prelude_files(tmp.path()).unwrap_err();
        assert!(
            err.contains("cannot read the prelude's load order"),
            "{err}"
        );
        // The index decides the order, not the file names.
        std::fs::write(
            dir.join("index.vlt"),
            "// Prelude.\nimport \"./b\";\nimport \"./a\";\n",
        )
        .unwrap();
        assert_eq!(
            prelude_files(tmp.path()).unwrap(),
            [dir.join("b.vlt"), dir.join("a.vlt")]
        );
        std::fs::write(dir.join("index.vlt"), "export { x } from \"./a\";\n").unwrap();
        let err = prelude_files(tmp.path()).unwrap_err();
        assert!(err.contains("line 1: expected `import"), "{err}");
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
