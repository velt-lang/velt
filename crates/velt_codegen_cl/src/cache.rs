//! Compiling a module's functions on every core, through a cache of compiled code shared between
//! builds ([`CodeCache`]).
//!
//! Functions translate and compile on worker threads (module.rs, parallel.rs). Cranelift keys a cached
//! function by its IR with the names it refers to left out, so a function compiles once per
//! machine however many programs contain it (std's functions and generic instances); the key
//! covers the target and every setting, and Cranelift rejects entries of another version.

use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::{CompiledCode, Context};

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

/// Compile `ctx`'s function, without the cache (the code stays in `ctx`); a cached compile is
/// [`compile_cached`].
pub(crate) fn compile_one(
    isa: &dyn TargetIsa,
    ctx: &mut Context,
    symbol: &str,
) -> CodegenResult<()> {
    let mut plane = ControlPlane::default();
    ctx.compile(isa, &mut plane)
        .map(|_| ())
        .map_err(|e| failed(e.func, symbol, e.inner))
}

/// The Cranelift that compiled the cached code. Its own key leaves the version out (its
/// `VersionMarker` hashes nothing), and an entry of another version is rejected and then never
/// replaced (the store keeps the first value of a key), so the version is in our key.
pub(crate) const CRANELIFT: &str = "cranelift-codegen 0.132.3";

/// Compile `ctx`'s function through `cache`, keyed by [`function_key`]: its code, and whether it
/// came from the cache. What `Context::compile_with_cache` does, with the key hashed by BLAKE3
/// instead of SHA-256 (its key was 14% of a warm debug build of a 3,000-function program, every
/// one a hit). The code is returned rather than kept in `ctx` (Cranelift keeps that field to
/// itself), so the callers take it from here.
pub(crate) fn compile_cached(
    isa: &dyn TargetIsa,
    ctx: &mut Context,
    symbol: &str,
    cache: &dyn CodeCache,
) -> CodegenResult<(CompiledCode, bool)> {
    use cranelift_codegen::incremental_cache::{serialize_compiled, try_finish_recompile};
    // Before `compile_stencil` optimizes the function in place, as Cranelift keys it.
    let key = store_key(CRANELIFT, &function_key(isa, &ctx.func));
    if let Some(blob) = cache.get(&key) {
        // An entry that doesn't decode (another Cranelift, a damaged file) is compiled again.
        if let Ok(code) = try_finish_recompile(&ctx.func, &blob) {
            return Ok((code, true));
        }
    }
    let mut plane = ControlPlane::default();
    // `compile_stencil` is public for tests and fuzzing in Cranelift; it is what
    // `compile_with_cache` runs on a miss.
    let stencil = ctx
        .compile_stencil(isa, &mut plane)
        .map_err(|e| failed(&ctx.func, symbol, e))?;
    let (stencil, blob) = serialize_compiled(stencil);
    if let Ok(blob) = blob {
        cache.put(&key, &blob);
    }
    Ok((stencil.apply_params(&ctx.func.params), false))
}

/// The keys [`compile_cached`] makes: BLAKE3 of what Cranelift's own key hashes. Different from
/// those of `compile_with_cache` (SHA-256), so an old entry is never taken for a new one.
const KEY_KIND: &[u8] = b"velt-blake3-1";

/// The store's key of a function whose [`function_key`] is `hash`, compiled by `cranelift`: its
/// version is in the key, since its own key leaves it out ([`CRANELIFT`]).
fn store_key(cranelift: &str, hash: &[u8; 32]) -> Vec<u8> {
    [cranelift.as_bytes(), b"\0", KEY_KIND, b"\0", hash].concat()
}

/// BLAKE3 of what Cranelift's `incremental_cache::CacheKey` hashes for `func` on `isa`: the
/// function's stencil (its IR without the names it refers to, with the version marker) and the
/// target's name, triple, flags and ISA flags. The same contents as Cranelift's key, so a hit is
/// exactly what Cranelift would have taken; buffered, so the hash runs once over the whole.
fn function_key(isa: &dyn TargetIsa, func: &cranelift_codegen::ir::Function) -> [u8; 32] {
    use std::hash::{Hash, Hasher};
    struct Bytes(Vec<u8>);
    impl Hasher for Bytes {
        fn finish(&self) -> u64 {
            // Only `write` is called: a `Hash` impl feeds a hasher, it doesn't read it.
            0
        }
        fn write(&mut self, bytes: &[u8]) {
            self.0.extend_from_slice(bytes);
        }
    }
    let mut b = Bytes(Vec::with_capacity(16 << 10));
    func.stencil.hash(&mut b);
    isa.name().hash(&mut b);
    isa.triple().hash(&mut b);
    isa.flags().hash(&mut b);
    isa.isa_flags_hash_key().hash(&mut b);
    *blake3::hash(&b.0).as_bytes()
}

/// A compile error of `func` as the build reports it.
fn failed(
    func: &cranelift_codegen::ir::Function,
    symbol: &str,
    e: cranelift_codegen::CodegenError,
) -> String {
    match e {
        cranelift_codegen::CodegenError::Verifier(errs) => {
            let msg = cranelift_codegen::print_errors::pretty_verifier_error(func, None, errs);
            format!("codegen: compiling function `{symbol}` failed: {msg}")
        }
        other => format!("codegen: compiling function `{symbol}` failed: {other:?}"),
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
        let f = [7u8; 32];
        map.put(&store_key("cranelift-codegen 1.0.0", &f), b"old code");
        let new = store_key("cranelift-codegen 1.0.1", &f);
        assert_eq!(map.get(&new), None);
        map.put(&new, b"new code");
        assert_eq!(map.get(&new).as_deref(), Some(&b"new code"[..]));
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
