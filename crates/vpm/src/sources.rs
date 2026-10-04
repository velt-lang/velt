//! Which files are Velt source modules: `.vlt`, and TypeScript's `.ts` and `.tsx` (so a folder
//! can be shared with a TypeScript project). Declaration files (`.d.ts`) are not modules. Every
//! tool that enumerates source files (the loader's relative imports, `velt check` in a package,
//! `velt test`, `velt fmt`, `velt doc`, the language server) uses these rules.
//!
//! A folder's source files are found by walking it ([`walks_into`]): below the folder, the walk
//! skips `target/`, `node_modules/`, hidden and symlinked directories, and nested packages (a
//! directory with its own manifest belongs to that package); a manifest is never a module.
//! [`in_folder`] answers the same question for one file, so the language server's idea of the
//! files under a `tsCompat` folder is the CLI's.

use std::path::{Path, PathBuf};

use crate::manifest::{LEGACY_MANIFEST_FILE, MANIFEST_FILE};

/// Directories a walk for source files never enters (besides hidden ones).
pub const SKIPPED_DIRS: [&str; 2] = ["target", "node_modules"];

/// Source file extensions, in the order a relative import tries them (`./x` → `x.vlt`, `x.ts`,
/// `x.tsx`).
pub const SOURCE_EXTENSIONS: [&str; 3] = ["vlt", "ts", "tsx"];

/// Whether a file named `name` is a source module (`a.vlt`, `a.ts`, `a.tsx`; not `a.d.ts`).
pub fn is_source_name(name: &str) -> bool {
    strip_source_extension(name).is_some()
}

/// Whether `path` names a source module ([`is_source_name`] of its file name).
pub fn is_source_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_source_name)
}

/// `name` without its source extension (`a.test.ts` → `a.test`), or `None` if it is not a
/// source module.
pub fn strip_source_extension(name: &str) -> Option<&str> {
    if name.ends_with(".d.ts") {
        return None;
    }
    SOURCE_EXTENSIONS.iter().find_map(|ext| {
        name.strip_suffix(ext)
            .and_then(|s| s.strip_suffix('.'))
            .filter(|stem| !stem.is_empty() && !stem.ends_with('/'))
    })
}

/// Whether a walk for source files below a folder enters its subdirectory `dir`: not a symlink
/// (which can lead back up, `src/up -> ..`, walking the package again or forever), not `target/`,
/// `node_modules/` or hidden, and not the root of a nested package.
pub fn walks_into(dir: &Path) -> bool {
    let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let link = std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink());
    !link
        && !SKIPPED_DIRS.contains(&name)
        && !name.starts_with('.')
        && !dir.join(MANIFEST_FILE).is_file()
        && !dir.join(LEGACY_MANIFEST_FILE).is_file()
}

/// Whether a walk for source files keeps a file named `name`: a source module that
/// is not a package manifest.
pub fn walk_keeps(name: &str) -> bool {
    name != MANIFEST_FILE && is_source_name(name)
}

/// Whether the walk of `folder` finds the source file `file`: `file` is under it, every directory
/// between them is one the walk enters ([`walks_into`]), and the walk keeps its name
/// ([`walk_keeps`]). Both paths are compared as given, so callers pass them in the same form
/// (both canonical, say).
pub fn in_folder(folder: &Path, file: &Path) -> bool {
    let Ok(rel) = file.strip_prefix(folder) else {
        return false;
    };
    let Some(name) = rel.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !walk_keeps(name) {
        return false;
    }
    let mut dir = folder.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    parts[..parts.len() - 1].iter().all(|part| {
        dir.push(part);
        walks_into(&dir)
    })
}

/// Whether `path` is a TypeScript file without JSX (`.ts`): JSX is allowed only in `.tsx` (and
/// `.vlt`) files, as in TypeScript.
pub fn is_plain_ts(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "ts")
}

/// The files a module path `base` (no extension) may name, in [`SOURCE_EXTENSIONS`] order:
/// `base.vlt`, `base.ts`, `base.tsx` (`types.d` names none of them as `types.d.ts`: a
/// declaration file is not a module).
pub fn source_files(base: &str) -> Vec<PathBuf> {
    SOURCE_EXTENSIONS
        .iter()
        .map(|ext| format!("{base}.{ext}"))
        .filter(|f| is_source_name(f))
        .map(PathBuf::from)
        .collect()
}

/// The file of a package's default module `rel` (`src/main.vlt` or `src/lib.vlt`), below `root`:
/// that file, or the same name with `.ts` or `.tsx`, so a package written in TypeScript needs no
/// `entry`. `Ok(None)` when none exists, an error naming them when more than one does (as for an
/// ambiguous import).
pub fn default_module(root: &Path, rel: &str) -> Result<Option<PathBuf>, String> {
    let base = rel.strip_suffix(".vlt").unwrap_or(rel);
    let found: Vec<PathBuf> = source_files(base)
        .into_iter()
        .filter(|f| root.join(f).is_file())
        .collect();
    match found.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(root.join(one))),
        many => {
            let names: Vec<String> = many.iter().map(|f| format!("`{}`", f.display())).collect();
            Err(format!(
                "the package has more than one `{base}` module: {} (rename or remove all but one of them)",
                names.join(", ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_modules_may_be_typescript() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("src")).unwrap();
        assert_eq!(default_module(root, "src/lib.vlt"), Ok(None));
        std::fs::write(root.join("src/lib.ts"), "").unwrap();
        assert_eq!(
            default_module(root, "src/lib.vlt"),
            Ok(Some(root.join("src/lib.ts")))
        );
        std::fs::write(root.join("src/lib.vlt"), "").unwrap();
        let err = default_module(root, "src/lib.vlt").unwrap_err();
        assert!(err.contains("`src/lib.vlt`, `src/lib.ts`"), "{err}");
        assert_eq!(
            source_files("types.d"),
            [PathBuf::from("types.d.vlt"), PathBuf::from("types.d.tsx")]
        );
    }

    #[test]
    fn source_names() {
        for name in ["a.vlt", "a.ts", "a.tsx", "a.test.ts", "dir/x.tsx"] {
            assert!(is_source_name(name), "{name}");
        }
        for name in ["a.d.ts", "a.js", "a.vltx", "ts", ".ts", "a.json", "a.mts"] {
            assert!(!is_source_name(name), "{name}");
        }
        assert_eq!(strip_source_extension("a.test.ts"), Some("a.test"));
        assert_eq!(strip_source_extension("dir/x.tsx"), Some("dir/x"));
        assert!(is_plain_ts(Path::new("x.ts")));
        assert!(!is_plain_ts(Path::new("x.tsx")));
    }

    #[test]
    fn folder_membership_follows_the_walk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for f in [
            "a.vlt",
            "sub/b.ts",
            "node_modules/c.ts",
            "target/d.vlt",
            ".hidden/e.vlt",
            "nested/package.vlt",
            "nested/f.vlt",
            "legacy/velt.toml",
            "legacy/g.vlt",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        assert!(in_folder(root, &root.join("a.vlt")));
        assert!(in_folder(root, &root.join("sub/b.ts")));
        // A file not saved yet counts by its name and folders.
        assert!(in_folder(root, &root.join("sub/new.vlt")));
        for f in [
            "node_modules/c.ts",
            "target/d.vlt",
            ".hidden/e.vlt",
            "nested/f.vlt",
            "nested/package.vlt",
            "legacy/g.vlt",
            "package.vlt",
            "x.d.ts",
            "x.json",
        ] {
            assert!(!in_folder(root, &root.join(f)), "{f}");
        }
        assert!(!in_folder(&root.join("sub"), &root.join("a.vlt")));
        // The folder itself may be anything: only what is below it counts.
        assert!(in_folder(&root.join("nested"), &root.join("nested/f.vlt")));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink("real", root.join("link")).unwrap();
        assert!(in_folder(root, &root.join("real/a.vlt")));
        assert!(!in_folder(root, &root.join("link/a.vlt")));
    }
}
