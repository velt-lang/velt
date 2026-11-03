//! Compiling a module's functions on every core, through a cache of compiled code shared between
//! builds ([`CodeCache`]).
//!
//! Functions translate and compile on worker threads (module.rs, parallel.rs). Cranelift keys a cached
//! function by its IR with the names it refers to left out, so a function compiles once per
//! machine however many programs contain it (std's functions and generic instances); the key
//! covers the target and every setting, and Cranelift rejects entries of another version.

use std::borrow::Cow;

use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::incremental_cache::CacheKvStore;
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::Context;

use crate::CodegenResult;

/// Compiled code shared between builds, keyed by content (`velt_build_cache` keeps it on disk).
/// Lookups and inserts come from several threads at once.
pub trait CodeCache: Sync {
    /// The value stored under `key`, if any.
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
    /// Store `value` under `key` (best effort: a cache that cannot store just misses later).
    fn put(&self, key: &[u8], value: &[u8]);
}

/// How the functions of one module were compiled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Functions taken from the cache.
    pub hits: usize,
    /// Functions compiled.
    pub misses: usize,
}

impl CacheStats {
    /// Add `other`'s counts.
    pub fn add(&mut self, other: CacheStats) {
        self.hits += other.hits;
        self.misses += other.misses;
    }

    /// The stats of items that were (`true`) or were not taken from the cache.
    pub fn of(hits: &[bool]) -> CacheStats {
        let found = hits.iter().filter(|&&h| h).count();
        CacheStats {
            hits: found,
            misses: hits.len() - found,
        }
    }
}

/// Compile `ctx`'s function; whether it came from the cache.
pub(crate) fn compile_one(
    isa: &dyn TargetIsa,
    ctx: &mut Context,
    symbol: &str,
    cache: Option<&dyn CodeCache>,
) -> CodegenResult<bool> {
    let mut plane = ControlPlane::default();
    let result = match cache {
        Some(cache) => ctx
            .compile_with_cache(isa, &mut Store(cache, CRANELIFT), &mut plane)
            .map(|(_, hit)| hit),
        None => ctx.compile(isa, &mut plane).map(|_| false),
    };
    result.map_err(|e| match e.inner {
        cranelift_codegen::CodegenError::Verifier(errs) => {
            let msg = cranelift_codegen::print_errors::pretty_verifier_error(e.func, None, errs);
            format!("codegen: compiling function `{symbol}` failed: {msg}")
        }
        other => format!("codegen: compiling function `{symbol}` failed: {other:?}"),
    })
}

/// The Cranelift that compiled the cached code. Its own key leaves the version out (its
/// `VersionMarker` hashes nothing), and an entry of another version is rejected and then never
/// replaced (the store keeps the first value of a key), so the version is in our key.
pub(crate) const CRANELIFT: &str = "cranelift-codegen 0.132.3";

/// A [`CodeCache`] as Cranelift's store, for one version of Cranelift.
struct Store<'a>(&'a dyn CodeCache, &'a str);

impl CacheKvStore for Store<'_> {
    fn get(&self, key: &[u8]) -> Option<Cow<'_, [u8]>> {
        self.0.get(&self.key(key)).map(Cow::Owned)
    }

    fn insert(&mut self, key: &[u8], val: Vec<u8>) {
        self.0.put(&self.key(key), &val);
    }
}

impl Store<'_> {
    /// Cranelift's `key` for this version of Cranelift.
    fn key(&self, key: &[u8]) -> Vec<u8> {
        [self.1.as_bytes(), b"\0", key].concat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Map(Mutex<std::collections::HashMap<Vec<u8>, Vec<u8>>>);

    impl CodeCache for Map {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.0.lock().unwrap().get(key).cloned()
        }

        fn put(&self, key: &[u8], value: &[u8]) {
            self.0
                .lock()
                .unwrap()
                .entry(key.to_vec())
                .or_insert(value.to_vec());
        }
    }

    /// After a Cranelift upgrade, code the old one compiled is a miss, and the new code is
    /// stored and then found.
    #[test]
    fn another_cranelift_misses_then_stores_its_own() {
        let map = Map::default();
        Store(&map, "cranelift-codegen 1.0.0").insert(b"f", b"old code".to_vec());
        let mut new = Store(&map, "cranelift-codegen 1.0.1");
        assert_eq!(new.get(b"f"), None);
        new.insert(b"f", b"new code".to_vec());
        assert_eq!(new.get(b"f").as_deref(), Some(&b"new code"[..]));
    }

    /// `CRANELIFT` names the Cranelift the workspace builds with.
    #[test]
    fn the_version_is_the_locked_one() {
        let lock = include_str!("../../../Cargo.lock");
        let at = lock.find("name = \"cranelift-codegen\"\n").unwrap();
        let version = lock[at..].lines().nth(1).unwrap();
        let version = version.trim_start_matches("version = ").trim_matches('"');
        assert_eq!(CRANELIFT, format!("cranelift-codegen {version}"));
    }
}
