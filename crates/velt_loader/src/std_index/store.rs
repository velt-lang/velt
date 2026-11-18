//! Building the std index from a std root, one package at a time, through the build cache.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use velt_build_cache::{Cache, Key, KeyHasher};
use velt_common::std_modules::CANONICAL_PREFIX;

use super::{codec, index_module, PackageIndex, StdIndex};
use crate::locate::Origin;
use crate::spec::{resolve_spec, ModuleRef};
use crate::std_ast::{self, StdAstCache, StdParser, NO_FILE};

/// The cache's kind for a package's interface (package-cache.md layer 1), whose first fields are
/// the std index.
pub const KIND: &str = "pkg-interface";

/// A std source file: where it is, its canonical module path and its contents.
pub(crate) struct StdFile {
    pub path: PathBuf,
    pub canonical: String,
    pub src: String,
}

/// The index of every package under `std_root`, each entry read from `cache` (std's parse cache:
/// the same cache and compiler identity) when it holds one for the package's sources, else built
/// from their parses and stored. `None`: everything is parsed and nothing stored. A file that
/// cannot be read is left out; a package with a file that does not parse is indexed from what
/// parsed, and not stored.
pub fn load(std_root: &Path, cache: Option<StdAstCache>) -> StdIndex {
    let parser = StdParser::new(cache);
    let prefix = cache.map(|c| prefix(&c.compiler));
    let origin = Origin::Std(std_root.to_path_buf());
    let mut files = vec![];
    source_files(&origin, std_root, "std", &mut files);
    let mut packages: BTreeMap<String, Vec<StdFile>> = BTreeMap::new();
    for (path, package) in files {
        let Ok(src) = vpm::probe::read_to_string(&path) else {
            continue;
        };
        let canonical = origin.canonical(&path);
        packages.entry(package).or_default().push(StdFile {
            path,
            canonical,
            src,
        });
    }
    let packages = packages
        .into_iter()
        .map(|(package, mut files)| {
            files.sort_by(|a, b| a.canonical.cmp(&b.canonical));
            let store = cache.zip(prefix.as_ref()).map(|(c, p)| (c.cache, p));
            package_index(std_root, &package, &files, &parser, store)
        })
        .collect();
    StdIndex { packages }
}

/// The part of every key that is the same for all packages: the kind, the compiler and the
/// encoding's format.
pub(crate) fn prefix(compiler: &Key) -> KeyHasher {
    let mut h = KeyHasher::new();
    h.part(KIND.as_bytes())
        .part(&compiler.0)
        .part(super::FORMAT);
    h
}

/// The key of package `package` with `files`: `prefix`, the package's path, and each file's
/// canonical path and contents (as a `std-ast` key covers them).
pub(crate) fn key(prefix: &KeyHasher, package: &str, files: &[StdFile]) -> Key {
    let mut h = prefix.clone();
    h.part(package.as_bytes());
    h.part(&(files.len() as u64).to_le_bytes());
    for f in files {
        h.part(f.canonical.as_bytes());
        h.part(&std_ast::contents(
            &f.src,
            vpm::sources::is_plain_ts(&f.path),
        ));
    }
    h.finish()
}

/// The index of `package` (`files`, sorted by canonical path): `store`'s entry when it has one
/// that decodes, else built and stored there.
pub(crate) fn package_index(
    std_root: &Path,
    package: &str,
    files: &[StdFile],
    parser: &StdParser,
    store: Option<(&Cache, &KeyHasher)>,
) -> PackageIndex {
    let key = store.map(|(_, prefix)| key(prefix, package, files));
    if let Some(((cache, _), key)) = store.zip(key.as_ref()) {
        let hit = cache.get_key(KIND, key).and_then(|b| codec::decode(&b));
        if let Some(hit) = hit.filter(|p| p.path == package) {
            return hit;
        }
    }
    let mut parsed_all = true;
    let modules = files
        .iter()
        .map(|f| {
            let plain_ts = vpm::sources::is_plain_ts(&f.path);
            let (module, diags) = parser.parse(NO_FILE, &f.src, plain_ts, true);
            parsed_all &= diags.is_empty();
            let dir = f.path.parent().unwrap_or(std_root);
            index_module(&f.canonical, &module, &|spec| {
                import_path(std_root, dir, spec)
            })
        })
        .collect();
    let index = PackageIndex {
        path: package.to_string(),
        modules,
    };
    if let (Some((cache, _)), Some(key), true) = (store, key, parsed_all) {
        cache.put_key(KIND, &key, &codec::encode(&index));
    }
    index
}

/// What [`super::ModuleIndex::imports`] keeps of `spec`, imported by a std file in `dir`: the
/// canonical path of the std module it names (no disk access: `x.velt` and `x/index.velt` have the
/// same one), else `spec` as written.
fn import_path(std_root: &Path, dir: &Path, spec: &str) -> String {
    match resolve_spec(spec, dir) {
        Ok(ModuleRef::Std { rel }) => format!("{CANONICAL_PREFIX}{rel}"),
        Ok(ModuleRef::Relative { file }) => Origin::Std(std_root.to_path_buf()).canonical(&file),
        _ => spec.to_string(),
    }
}

/// Is `path` a std module: a source file, not one of std's own tests (`*.test.velt`, which no
/// program loads)?
fn is_module(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    vpm::sources::strip_source_extension(&name).is_some() && !vpm::sources::is_test_file(&name)
}

/// Every std module (`is_module`) below `dir`, with the canonical path of the package it
/// belongs to: the nearest folder above it with an `index.velt`, else `package`.
fn source_files(origin: &Origin, dir: &Path, package: &str, out: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = vpm::probe::read_dir(dir) else {
        return;
    };
    let index = dir.join("index.velt");
    let package = match entries.iter().any(|e| e.path() == index) {
        true => origin.canonical(&index),
        false => package.to_string(),
    };
    for e in entries {
        let path = e.path();
        let is_dir = match e.file_type() {
            Ok(t) if !t.is_symlink() => t.is_dir(),
            _ => vpm::probe::is_dir(&path),
        };
        if is_dir {
            source_files(origin, &path, &package, out);
        } else if is_module(&path) {
            out.push((path, package.clone()));
        }
    }
}
