//! EXPERIMENT (`VELT_UNITS_DISK=<dir>`, #2943 M2): std's unit kept on disk. With a check by units
//! (which loads every std file, `commands::check::with_all_std`), the driver keys std's sources
//! before loading (the compiler and std's files, [`std_tree`]) and, when the stubs of std's
//! modules are on disk under that key (`velt_sema::read_std_stubs`), loads them instead of
//! parsing std: sema takes the rest of std from the unit (`CheckOptions::std_stubbed`). When no
//! unit fits the check, sema says so (`velt_sema::STD_UNIT_NEEDED`) and the driver loads std and
//! checks again, which saves one.
//!
//! std's files are a snapshot kept beside the unit (`velt_loader::std_tree`): checked with a
//! `stat` per directory and file, it lists std's files for the load and, with stubs, answers
//! the loader's questions about std's paths, so the load reads no std file.

use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use std::collections::HashMap;
use velt_loader as loader;
use velt_loader::std_tree::StdTree;

use super::CheckScope;

/// std's sources' key, with the stubs kept under it if any.
pub(super) struct Prepared {
    pub key: u64,
    pub stubs: Option<HashMap<String, Vec<u8>>>,
}

/// The directory std's unit is kept in, when the experiment is on.
fn dir() -> Option<PathBuf> {
    std::env::var_os("VELT_UNITS_DISK").map(PathBuf::from)
}

/// std's files as they are now (the snapshot beside the unit when it is still true, else a new
/// one, which replaces it), once per process; `None` without the experiment or when std cannot
/// be walked.
pub(crate) fn std_tree() -> Option<&'static StdTree> {
    static TREE: OnceLock<Option<StdTree>> = OnceLock::new();
    TREE.get_or_init(|| {
        let dir = dir()?;
        let std = loader::std_root()?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (crate::build_cache::compiler_identity().0, vpm::relpath::absolute(&std)).hash(&mut h);
        let path = dir.join(format!("{:016x}.tree", std::hash::Hasher::finish(&h)));
        let kept = (std::fs::read_to_string(&path).ok())
            .and_then(|text| StdTree::decode(&text))
            .filter(StdTree::fresh);
        if kept.is_some() {
            return kept;
        }
        let tree = StdTree::walk(&std)?;
        write_whole(&path, tree.encode().as_bytes());
        Some(tree)
    })
    .as_ref()
}

pub(super) fn prepare(scope: &CheckScope) -> Option<Prepared> {
    if !scope.units {
        return None;
    }
    let dir = dir()?;
    let tree = std_tree()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    crate::build_cache::compiler_identity().0.hash(&mut h);
    // Every std file's path, length and time of change (not its contents: reading every std
    // file to key it costs more than the rest of the load).
    tree.encode().hash(&mut h);
    let key = std::hash::Hasher::finish(&h);
    let stubs: Option<HashMap<String, Vec<u8>>> =
        velt_sema::read_std_stubs(&dir, key).map(|s| s.into_iter().collect());
    if let Some(stubs) = &stubs {
        let stubbed = stubs.keys().cloned().collect();
        loader::std_tree::install(tree.clone(), stubbed);
    }
    Some(Prepared { key, stubs })
}

/// Write `bytes` to `path` whole: to a temporary file, then renamed into place.
fn write_whole(path: &Path, bytes: &[u8]) {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = path
        .parent()
        .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
        && std::fs::write(&tmp, bytes).is_ok()
        && std::fs::rename(&tmp, path).is_ok();
    if !written {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Give the files of `diags`' locations that a load with std's stubs registered without their
/// sources (std's, `velt_loader::std_tree`) their sources, so the messages show them: the
/// source map is rebuilt with the same files in the same order.
pub(crate) fn fill_sources<'d>(
    sm: &mut velt_common::SourceMap,
    diags: impl IntoIterator<Item = &'d velt_common::Diagnostic>,
) {
    let wanted: std::collections::HashSet<velt_common::FileId> = (diags.into_iter())
        .flat_map(|d| d.labels.iter().map(|l| l.span.file))
        .filter(|&f| (f.0 as usize) < sm.files().count() && sm.get(f).src.is_empty())
        .collect();
    if wanted.is_empty() {
        return;
    }
    let mut filled = velt_common::SourceMap::new();
    for (id, f) in sm.files() {
        let src = match wanted.contains(&id) {
            true => std::fs::read_to_string(&f.path).unwrap_or_default(),
            false => f.src.clone(),
        };
        filled.add(f.path.clone(), src);
    }
    *sm = filled;
}
