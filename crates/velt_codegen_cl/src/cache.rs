//! Compiling a module's functions on every core, through a cache of compiled code shared between
//! builds ([`CodeCache`]).
//!
//! Translation into Cranelift IR stays sequential (it declares into the module); compilation,
//! most of the backend's time, runs per function on worker threads. Cranelift keys a cached
//! function by its IR with the names it refers to left out, so a function compiles once per
//! machine however many programs contain it (std's functions and generic instances); the key
//! covers the target and every setting, and Cranelift rejects entries of another version.

use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

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
    /// The stats of items that were (`true`) or were not taken from the cache.
    pub fn of(hits: &[bool]) -> CacheStats {
        let found = hits.iter().filter(|&&h| h).count();
        CacheStats {
            hits: found,
            misses: hits.len() - found,
        }
    }
}

/// Below this many functions a module compiles on the calling thread: starting threads costs
/// more than it saves.
const PARALLEL_MIN_FUNCTIONS: usize = 32;

/// Compile every context's function for `isa`, through `cache` when there is one; on every core
/// for larger modules. The compiled code is left in each context. An error names the function.
pub(crate) fn compile_all(
    isa: &dyn TargetIsa,
    ctxs: &mut [Context],
    symbols: &[&str],
    cache: Option<&dyn CodeCache>,
) -> CodegenResult<CacheStats> {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let workers = if ctxs.len() < PARALLEL_MIN_FUNCTIONS {
        1
    } else {
        cores.min(ctxs.len())
    };
    let hits = AtomicUsize::new(0);
    let compile = |ctx: &mut Context, symbol: &str| -> CodegenResult<()> {
        let hit = compile_one(isa, ctx, symbol, cache)?;
        if hit {
            hits.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    };
    if workers == 1 {
        for (ctx, symbol) in ctxs.iter_mut().zip(symbols) {
            compile(ctx, symbol)?;
        }
    } else {
        // Workers take the next function left, so a few large ones don't serialize the rest.
        let queue = Mutex::new(ctxs.iter_mut().zip(symbols).enumerate());
        let first_error = Mutex::new(None::<(usize, String)>);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    let next = queue.lock().unwrap_or_else(PoisonError::into_inner).next();
                    let Some((i, (ctx, symbol))) = next else {
                        return;
                    };
                    if let Err(e) = compile(ctx, symbol) {
                        let mut first = first_error.lock().unwrap_or_else(PoisonError::into_inner);
                        if first.as_ref().is_none_or(|(j, _)| i < *j) {
                            *first = Some((i, e));
                        }
                    }
                });
            }
        });
        let first = first_error
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((_, e)) = first {
            return Err(e);
        }
    }
    let hits = hits.into_inner();
    Ok(CacheStats {
        hits,
        misses: ctxs.len() - hits,
    })
}

/// Compile `ctx`'s function; whether it came from the cache.
fn compile_one(
    isa: &dyn TargetIsa,
    ctx: &mut Context,
    symbol: &str,
    cache: Option<&dyn CodeCache>,
) -> CodegenResult<bool> {
    let mut plane = ControlPlane::default();
    let result = match cache {
        Some(cache) => ctx
            .compile_with_cache(isa, &mut Store(cache), &mut plane)
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

/// A [`CodeCache`] as Cranelift's store.
struct Store<'a>(&'a dyn CodeCache);

impl CacheKvStore for Store<'_> {
    fn get(&self, key: &[u8]) -> Option<Cow<'_, [u8]>> {
        self.0.get(key).map(Cow::Owned)
    }

    fn insert(&mut self, key: &[u8], val: Vec<u8>) {
        self.0.put(key, &val);
    }
}
