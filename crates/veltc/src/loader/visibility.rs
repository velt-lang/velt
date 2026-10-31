//! Which std files a module may load (docs/internals/design/std-layout.md §2, §5, §9).
//!
//! User code (any module not loaded from std, packages included) imports only std's modules: a
//! folder's `index.vlt` outside `prelude/` and `lang/` ([`std_modules::importable`]). Any other
//! std file is internal to the module whose folder holds it, and importing it is an error that
//! names that module. std code imports any std file. The import the parser adds for
//! `import.meta` is the compiler's, not the user's.
//!
//! std's own test files (`std/<module>/<name>.test.vlt`, the white-box tests `velt test std/`
//! runs) are std code wherever the import that reaches them comes from: the generated harness of
//! `velt test` imports them relatively, and they import the internals they test.

use std::path::Path;

use velt_common::std_modules;

use super::locate::{module_path, Origin, Target};
use super::Loader;
use crate::commands::test::discover::is_test_file;

impl Loader<'_, '_> {
    /// `target` with a std origin when it is one of std's test files (`file`, whose
    /// [`super::file_key`] is `key`) reached from outside std.
    pub(super) fn std_test_origin(&self, target: Target, file: &Path, key: &Path) -> Target {
        let (Some(std), Some(std_key)) = (&self.opts.std_root, &self.std_key) else {
            return target;
        };
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let test = is_test_file(name);
        if matches!(target.origin, Origin::Std(_)) || !test || !key.starts_with(std_key) {
            return target;
        }
        Target {
            canonical: None,
            origin: Origin::Std(std.clone()),
            ..target
        }
    }

    /// Whether module `from` (an importer's origin) may load the std file `key` (a
    /// [`super::file_key`]) that `spec` names: `Err` with the message and notes when it is
    /// internal.
    pub(super) fn visible(
        &self,
        from: &Origin,
        target: &Target,
        key: &Path,
        spec: &str,
    ) -> Result<(), (String, Vec<String>)> {
        let (Some(std), Some(std_key)) = (&self.opts.std_root, &self.std_key) else {
            return Ok(());
        };
        if matches!(from, Origin::Std(_)) || !matches!(target.origin, Origin::Std(_)) {
            return Ok(());
        }
        let rel = vpm::relpath::relative(key, std_key);
        // std's test files: `velt test std/`'s harness imports them ([`Self::std_test_origin`]).
        let test = rel.rsplit('/').next().is_some_and(is_test_file);
        if !key.starts_with(std_key) || std_modules::importable(&rel) || test {
            return Ok(());
        }
        let shown = format!("{}{}", std_modules::SPECIFIER_PREFIX, module_path(&rel));
        let shown = if spec.starts_with(std_modules::SPECIFIER_PREFIX) {
            spec
        } else {
            &shown
        };
        Err(internal(&rel, shown, &|dir| {
            vpm::probe::is_file(&std.join(dir).join("index.vlt"))
        }))
    }

    /// A module is std exactly when its file lies below the std root (`key`: its
    /// [`super::file_key`], so symlinks and junctions are resolved): a `velt/` path must not leave
    /// the root, and user code must not load a std file under a non-std origin (deduplication
    /// would hand that copy to std's own imports).
    pub(super) fn std_membership(
        &self,
        origin: &Origin,
        key: &Path,
        spec: &str,
    ) -> Result<(), String> {
        let Some(std_key) = &self.std_key else {
            return Ok(());
        };
        let inside = key.starts_with(std_key);
        match origin {
            Origin::Std(_) if !inside => Err(format!(
                "module `{spec}` resolves to a file outside the standard library"
            )),
            Origin::Std(_) => Ok(()),
            _ if inside => {
                let rel = vpm::relpath::relative(key, std_key);
                let rel = module_path(&rel);
                Err(format!(
                    "module `{spec}` is a file of the standard library: import it as `velt/{rel}`"
                ))
            }
            _ => Ok(()),
        }
    }
}

/// Is `rel` (below the std root) the module `folder` or a file in it?
fn in_folder(rel: &str, folder: &str) -> bool {
    rel.strip_prefix(folder)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The error for a user module's import of the internal std file `rel` (below the std root)
/// written as `spec`; `has_index` says whether a folder below the std root is a module.
fn internal(rel: &str, spec: &str, has_index: &dyn Fn(&str) -> bool) -> (String, Vec<String>) {
    let module = |m: &str| format!("{}{m}", std_modules::SPECIFIER_PREFIX);
    match rel.split('/').next().unwrap_or("") {
        "prelude" => (
            format!("`{spec}` is part of the prelude, which no module imports"),
            vec!["its names are in scope in every module without an import".into()],
        ),
        "lang" if in_folder(rel, velt_common::std_rel!(lang_intrinsics)) => (
            format!("`{spec}` is internal to the standard library: the compiler's intrinsics are std's own building blocks"),
            vec![
                "use the public API std builds on them (an array's `reserve`, `truncate` and `swap`, velt/encoding's `utf8ByteAt`, …)"
                    .into(),
            ],
        ),
        "lang" => (
            format!("`{spec}` is internal to the standard library: `lang/` implements the language"),
            vec![
                "its classes and functions are globals (`Symbol`, `Proxy`, `Uint8Array`, …): use them without an import"
                    .into(),
            ],
        ),
        _ => match std_modules::owner(rel, has_index) {
            Some(owner) => (
                format!("`{spec}` is internal to `{}`", module(&owner)),
                vec![format!(
                    "import what you need from \"{}\", the module's public API",
                    module(&owner)
                )],
            ),
            None => (
                format!("`{spec}` is internal to the standard library"),
                vec![
                    "user code imports a module's `index.vlt` (docs/std/README.md lists the modules)"
                        .into(),
                ],
            ),
        },
    }
}
