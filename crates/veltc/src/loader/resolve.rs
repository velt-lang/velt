//! Resolving one import specifier to its file without loading anything, for the language
//! server's import help: the same classification ([`spec`](super::spec)), candidates
//! ([`locate`](super::locate)), case-exact picking and ambiguity ([`pick`](super::pick)) and std
//! membership as [`super::load_program`], with no diagnostics: a specifier that would be an error
//! there resolves to nothing here.

use std::path::{Path, PathBuf};

use super::case::DirNames;
use super::file_key;
use super::locate::{self, Origin, PackageResolver};
use super::pick::{first_existing, on_disk_exactly};
use super::spec::{resolve_spec, ModuleRef};

/// The file that `spec`, imported by the file `importer`, names; `None` when the import would
/// fail (invalid, not found, ambiguous, outside the std root, a std file named by path).
pub fn resolve_module(
    spec: &str,
    importer: &Path,
    std_root: Option<&Path>,
    packages: Option<&dyn PackageResolver>,
) -> Option<PathBuf> {
    let dir = importer.parent()?;
    let aliased =
        (!spec.starts_with("./") && !spec.starts_with("../") && !spec.starts_with("velt:"))
            .then(|| packages?.path_alias(importer, spec))
            .flatten();
    let module = match aliased {
        Some(file) => ModuleRef::Relative {
            file: vpm::relpath::normalize(&file),
        },
        None => resolve_spec(spec, dir).ok()?,
    };
    let origin = Origin::Root(dir.to_path_buf());
    let target = locate::target(module, importer, &origin, std_root, packages).ok()?;
    let names = DirNames::default();
    let (_, files) = first_existing(&target, |f| on_disk_exactly(&names, f, &target.base))?;
    let [file] = files.as_slice() else {
        return None;
    };
    let file = (*file).clone();
    if let Some(std) = std_root {
        let inside = file_key(&file).starts_with(file_key(std));
        if inside != matches!(target.origin, Origin::Std(_)) {
            return None;
        }
    }
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn resolves_like_the_loader() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let std = root.join("std");
        for f in [
            "std/fs.vlt",
            "std/collections/set.vlt",
            "app/util.vlt",
            "app/dup.vlt",
            "app/dup.ts",
            "app/shapes/index.vlt",
            "secret.vlt",
        ] {
            write(&root, f);
        }
        let main = root.join("app/main.vlt");
        let resolve = |spec: &str| resolve_module(spec, &main, Some(&std), None);
        assert_eq!(resolve("velt:fs"), Some(std.join("fs.vlt")));
        assert_eq!(
            resolve("velt:collections/set"),
            Some(std.join("collections/set.vlt"))
        );
        assert_eq!(resolve("./util"), Some(root.join("app/util.vlt")));
        assert_eq!(resolve("./shapes"), Some(root.join("app/shapes/index.vlt")));
        assert_eq!(resolve("./dup.ts"), Some(root.join("app/dup.ts")));
        // Ambiguous, wrong case, missing, or escaping the std root: nothing.
        for spec in [
            "./dup",
            "./Util",
            "./nope",
            "velt:../secret",
            "velt:..\\secret",
            "velt:C:/secret",
            "velt:fs\\..\\..\\secret",
            ".\\util",
            "../std/fs",
            "json",
        ] {
            assert_eq!(resolve(spec), None, "{spec}");
        }
    }
}
