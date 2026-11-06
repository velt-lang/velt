//! Resolving one import specifier to its file without loading anything, for the language
//! server's import help: the same classification ([`spec`](super::spec)), candidates
//! ([`locate`](super::locate)), case-exact picking and ambiguity ([`pick`](super::pick)) and std
//! membership as [`super::load_program`], with no diagnostics: a specifier that would be an error
//! there resolves to nothing here.

use std::path::{Path, PathBuf};

use super::case::DirNames;
use super::locate::{self, Origin, PackageResolver};
use super::pick::{first_existing, on_disk_exactly};
use super::spec::{resolve_spec, ModuleRef};
use super::{file_key, is_std_path};

/// The file that `spec`, imported by the file `importer`, names in a build for `target` (`None`:
/// the host; it selects a `#name`'s variant); `None` when the import would fail (invalid, not
/// found, ambiguous, outside the std root, a std file named by path).
pub fn resolve_module(
    spec: &str,
    importer: &Path,
    std_root: Option<&Path>,
    packages: Option<&dyn PackageResolver>,
    target: Option<&str>,
) -> Option<PathBuf> {
    let dir = importer.parent()?;
    let in_std = std_root.is_some_and(|std| file_key(importer).starts_with(file_key(std)));
    let aliased = if vpm::imports::is_specifier(spec) {
        let std = std_root.filter(|_| in_std);
        Some(super::conditional::resolve(
            spec, importer, std, packages, target,
        )?)
    } else {
        (!spec.starts_with("./") && !spec.starts_with("../") && !spec.starts_with("velt/"))
            .then(|| packages?.path_alias(importer, spec))
            .flatten()
    };
    let module = match aliased {
        Some(file) => ModuleRef::Relative {
            file: vpm::relpath::normalize(&file),
        },
        None => {
            let classified = resolve_spec(spec, dir);
            let mut npm = vpm::npm::NpmResolver::default();
            match super::locate::npm_module(&mut npm, packages, importer, spec, &classified) {
                Some(found) => ModuleRef::Relative { file: found.ok()? },
                None => classified.ok()?,
            }
        }
    };
    let origin = match std_root {
        Some(std) if in_std => Origin::Std(std.to_path_buf()),
        _ => Origin::Root(dir.to_path_buf()),
    };
    let target = locate::target(module, importer, &origin, std_root, packages).ok()?;
    let names = DirNames::default();
    let (_, files) = first_existing(&target, |f| on_disk_exactly(&names, f, &target.base))?;
    let [file] = files.as_slice() else {
        return None;
    };
    let file = (*file).clone();
    // A user module whose module path would be `std/…` is reserved for the standard library.
    let canonical = target
        .canonical
        .clone()
        .unwrap_or_else(|| target.origin.canonical(&file));
    if !matches!(target.origin, Origin::Std(_)) && is_std_path(&canonical) {
        return None;
    }
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
            "std/fs/index.velt",
            "std/net/index.velt",
            "std/net/bytes.velt",
            "app/std/x.velt",
            "std/collections/set.velt",
            "app/util.velt",
            "app/dup.velt",
            "app/dup.ts",
            "app/shapes/index.velt",
            "secret.velt",
        ] {
            write(&root, f);
        }
        let main = root.join("app/main.velt");
        let resolve = |spec: &str| resolve_module(spec, &main, Some(&std), None, None);
        assert_eq!(resolve("velt/fs"), Some(std.join("fs/index.velt")));
        assert_eq!(
            resolve("velt/collections/set"),
            Some(std.join("collections/set.velt"))
        );
        assert_eq!(resolve("./util"), Some(root.join("app/util.velt")));
        assert_eq!(resolve("./shapes"), Some(root.join("app/shapes/index.velt")));
        assert_eq!(resolve("./dup.ts"), Some(root.join("app/dup.ts")));
        // From a std file, relative imports stay in std.
        let net = std.join("net.velt");
        assert_eq!(
            resolve_module("./net/bytes", &net, Some(&std), None, None),
            Some(std.join("net/bytes.velt"))
        );
        // Ambiguous, wrong case, missing, or escaping the std root: nothing.
        for spec in [
            "./dup",
            "./Util",
            "./nope",
            "velt:../secret",
            "velt:..\\secret",
            "velt:C:/secret",
            "velt/fs\\..\\..\\secret",
            ".\\util",
            "../std/fs",
            "./std/x",
            "json",
        ] {
            assert_eq!(resolve(spec), None, "{spec}");
        }
    }
}
