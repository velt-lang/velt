//! How the loader names files: their identity for deduplication ([`file_key`]), how messages
//! show them, and which canonical module paths are std's.

use std::path::{Path, PathBuf};

use velt_common::std_modules;

/// Whether canonical module path `path` is in the standard library's namespace (`std`, `std/…`).
pub(crate) fn is_std_path(path: &str) -> bool {
    path == "std" || std_modules::is_std(path)
}

/// `file` as shown in a message: relative to its package's root (the directory with
/// `package.vlt`), or as given outside a package.
pub(crate) fn shown_in_package(file: &Path) -> String {
    let dir = file.parent().unwrap_or(Path::new(""));
    match vpm::manifest::find_package_root(dir) {
        Some(root) => vpm::relpath::relative(&vpm::relpath::absolute(file), &root),
        None => file.display().to_string(),
    }
}

/// Identity of a file for deduplication (canonical path when it exists). A file that does not
/// exist (a virtual root: the generator host beside std's modules, `velt test`'s harness) is
/// named below its directory's canonical path, so it compares with the keys of real files and
/// of the std root: on Windows a canonical path is a verbatim one (`\\?\C:\…`), and on any
/// system the directory may be reached through a link.
pub(crate) fn file_key(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    let abs = vpm::relpath::absolute(path);
    match (abs.parent(), abs.file_name()) {
        (Some(dir), Some(name)) => std::fs::canonicalize(dir).map_or(abs.clone(), |d| d.join(name)),
        _ => abs,
    }
}

/// How a message names `file`, a candidate for an import in directory `dir`: relative to `dir`
/// for relative (and path alias) imports (`dup.ts`, `../lib/x.vlt`), the full path otherwise.
pub(crate) fn shown_path(file: &Path, dir: &Path, relative: bool) -> String {
    if relative {
        vpm::relpath::relative(file, dir)
    } else {
        file.display().to_string()
    }
}
