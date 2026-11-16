//! EXPERIMENT (`VELT_UNITS_DISK=<dir>`, #2943 M2): std's unit kept on disk. With a check by units
//! (which loads every std file, `commands::check::with_all_std`), the driver keys std's sources
//! before loading (the compiler, and each std file's path, length and time of change) and, when the stubs of
//! std's modules are on disk under that key (`velt_sema::read_std_stubs`), loads them instead
//! of parsing std: sema takes the rest of std from the unit (`CheckOptions::std_stubbed`). When
//! no unit fits the check, sema says so (`velt_sema::STD_UNIT_NEEDED`) and the driver loads std
//! and checks again, which saves one.

use std::hash::Hash;
use std::path::Path;

use std::collections::HashMap;
use velt_loader as loader;

use super::CheckScope;

/// std's sources' key, with the stubs kept under it if any.
pub(super) struct Prepared {
    pub key: u64,
    pub stubs: Option<HashMap<String, Vec<u8>>>,
}

pub(super) fn prepare(scope: &CheckScope) -> Option<Prepared> {
    if !scope.units {
        return None;
    }
    let dir = std::env::var_os("VELT_UNITS_DISK")?;
    let std = loader::std_root()?;
    let mut files: Vec<&Path> = (scope.extra_roots.iter())
        .map(|p| p.as_path())
        .filter(|p| p.starts_with(&std))
        .collect();
    files.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    crate::build_cache::compiler_identity().0.hash(&mut h);
    // Each file's path, length and time of change (not its contents: reading every std file
    // to key it costs more than the rest of the load).
    for f in files {
        f.strip_prefix(&std).unwrap_or(f).hash(&mut h);
        let meta = std::fs::metadata(f).ok();
        let changed = meta.as_ref().and_then(|m| m.modified().ok());
        (meta.map(|m| m.len()), changed).hash(&mut h);
    }
    let key = std::hash::Hasher::finish(&h);
    let stubs = velt_sema::read_std_stubs(Path::new(&dir), key);
    Some(Prepared { key, stubs })
}
