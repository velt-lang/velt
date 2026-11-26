//! The program with every std file: `VELT_STD_SUMMARY_EAGER` (the std identity oracle's
//! whole-std mode) and a check by units (`VELT_UNITS`, velt_sema `units`, which checks std whole
//! as its own unit). `velt check`, `velt build`, `velt run` and `velt test` take this scope alike
//! when either is set, so a build by units lowers the program the check by units checked.

use std::path::{Path, PathBuf};

use super::{BuildError, BuildOptions, CheckScope};

/// Is a check by units or the oracle's whole-std mode asked for?
pub(crate) fn wanted() -> bool {
    ["VELT_STD_SUMMARY_EAGER", "VELT_UNITS"]
        .iter()
        .any(|v| std::env::var_os(v).is_some())
}

/// `scope` with every std file, each std definition checked whether or not the program reaches
/// it, as a cache of checked std holds std; by units when `VELT_UNITS` is set. A source
/// generator's host is built apart and doesn't see it.
pub(crate) fn with_all_std(
    mut scope: CheckScope,
    opts: &BuildOptions,
) -> Result<CheckScope, BuildError> {
    let std = velt_loader::std_root().ok_or_else(|| {
        BuildError::Failed("cannot find the standard library; set VELT_STD".into())
    })?;
    let imports = std.join(vpm::imports::STD_IMPORTS_FILE);
    let mut files = std_files(&std)?;
    if let Ok(src) = std::fs::read_to_string(&imports) {
        let map = vpm::manifest::read::read_std_imports(velt_common::FileId(0), &src);
        let unused = unselected_variants(&std, &map.unwrap_or_default(), &opts.target());
        files.retain(|f| !unused.contains(&file_key(f)));
    }
    scope.extra_roots.extend(files);
    scope.eager_std = true;
    scope.units = std::env::var_os("VELT_UNITS").is_some();
    Ok(scope)
}

/// Every file of the standard library at `std` (its tests and its import map aside), sorted.
pub(crate) fn std_files(std: &Path) -> Result<Vec<PathBuf>, BuildError> {
    let mut files: Vec<PathBuf> = crate::commands::test::discover::source_files(std)
        .map_err(BuildError::Failed)?
        .into_iter()
        .filter(|f| {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned());
            name.is_some_and(|n| n.ends_with(".velt") && !n.ends_with(".test.velt"))
        })
        .collect();
    // std's conditional imports are data the loader reads, as a manifest is, not a module.
    let imports = std.join(vpm::imports::STD_IMPORTS_FILE);
    files.retain(|f| *f != imports);
    files.sort();
    Ok(files)
}

/// The files of `imports` (variants relative to `base`) that a build for `target` uses for no
/// `#name`, as [`file_key`]s.
pub(crate) fn unselected_variants(
    base: &Path,
    imports: &vpm::imports::ImportMap,
    target: &str,
) -> Vec<PathBuf> {
    let conditions = vpm::imports::conditions(target, None);
    let file = |t: &str| file_key(&base.join(t.trim_start_matches("./")));
    let used: Vec<PathBuf> = imports
        .values()
        .filter_map(|variants| vpm::imports::select(variants, &conditions))
        .map(|v| file(&v.target))
        .collect();
    imports
        .values()
        .flatten()
        .map(|v| file(&v.target))
        .filter(|f| !used.contains(f))
        .collect()
}

pub(crate) fn file_key(f: &Path) -> PathBuf {
    std::fs::canonicalize(f).unwrap_or_else(|_| f.to_path_buf())
}
