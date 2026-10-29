//! Compiling a std function on its own, and storing its object.
//!
//! The function is compiled in a copy of the program, the *shell*, where every other function is
//! a declaration (`Import`), small ones keeping their bodies so it can still inline them (they
//! are in its key). It is exported, optimized with unknown callers (velt_opt treats a declaration
//! as opaque, and an exported function as called from anywhere), and compiled by LLVM to an
//! object of its own. The program's entry is declared, never defined, in such an object
//! (vir.rs invariant 5).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use velt_build_cache::{Cache, Key};
use velt_vir::vir::{self, Linkage};

use super::{select, KIND};

/// Compile each function of `misses` on its own, in parallel: its object, or `None` when it
/// could not be (it then stays in the program).
pub(super) fn each(
    program: &vir::Program,
    misses: &[(usize, Key)],
    cg: &velt_codegen_cl::CodegenOptions,
) -> Vec<Option<Vec<u8>>> {
    let shell = shell(program);
    let next = AtomicUsize::new(0);
    let results = Mutex::new(vec![None; misses.len()]);
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|scope| {
        for _ in 0..workers.min(misses.len()) {
            scope.spawn(|| loop {
                let n = next.fetch_add(1, Ordering::Relaxed);
                let Some(&(i, _)) = misses.get(n) else {
                    return;
                };
                let object = alone(program, &shell, i, cg);
                results.lock().unwrap_or_else(PoisonError::into_inner)[n] = object;
            });
        }
    });
    results.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// `program` with every function a declaration, small internal ones keeping their bodies, and no
/// source locations (velt_opt wants all functions with them or none).
fn shell(program: &vir::Program) -> vir::Program {
    let mut shell = program.clone();
    for f in &mut shell.funcs {
        if !select::small(f) || f.linkage != Linkage::Internal {
            f.blocks.clear();
            f.locals.truncate(f.params.len());
        }
        f.locs.clear();
        f.linkage = Linkage::Import;
    }
    shell.files.clear();
    shell
}

/// Function `i` of `program` compiled on its own, with unknown callers.
fn alone(
    program: &vir::Program,
    shell: &vir::Program,
    i: usize,
    cg: &velt_codegen_cl::CodegenOptions,
) -> Option<Vec<u8>> {
    let mut alone = shell.clone();
    let mut f = program.funcs[i].clone();
    f.linkage = Linkage::Export;
    f.locs.clear();
    alone.funcs[i] = f;
    velt_opt::optimize(&mut alone, velt_opt::OptLevel::Speed);
    // The entry stays declared (`dead_funcs` drops a declaration nothing calls).
    if !alone.funcs.iter().any(|f| f.symbol == "velt_main") {
        let main = vir::Function::new("velt_main".into(), vec![], vir::Ty::I32, Linkage::Import);
        alone.funcs.push(main);
    }
    let not_compiled = |why: String| {
        if super::stats() {
            eprintln!(
                "velt: std cache: {} stays in the program: {why}",
                program.funcs[i].symbol
            );
        }
    };
    if let Err(errs) = velt_vir::verify(&alone) {
        not_compiled(errs.join("; "));
        return None;
    }
    let mut steps = vec![];
    match crate::backend::Backend::Llvm.emit_objects(&alone, cg, Some(1), &mut steps) {
        Ok((objects, _)) => objects.into_iter().next(),
        Err(e) => {
            not_compiled(e);
            None
        }
    }
}

/// Store `object` under `key`: the path of the cached file, which the link reads. Two builds
/// storing one key at once both get the one valid entry (`Cache::put_file`).
pub(super) fn store(cache: &Cache, key: &Key, object: &[u8]) -> Option<PathBuf> {
    let name = format!("velt-stdfn-{}-{}.o", std::process::id(), key.hex());
    let tmp = std::env::temp_dir().join(name);
    std::fs::write(&tmp, object).ok()?;
    let stored = cache.put_file(KIND, key, &tmp);
    let _ = std::fs::remove_file(&tmp);
    stored.then(|| cache.path_of(KIND, key)).flatten()
}
