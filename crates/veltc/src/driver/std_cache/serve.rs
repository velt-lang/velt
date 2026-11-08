//! A debug build's use of the std cache: objects already compiled, optimized, stand in for std
//! functions, and everything else goes on as in any debug build.
//!
//! A function whose key is in the cache becomes a declaration, and its object goes to the link. A
//! miss is no failure and costs nothing: the function stays in the program, and Cranelift compiles
//! it with the rest. A debug build never starts clang, which may not be installed. Only
//! `velt cache warm` stores objects (`VELT_STD_OBJECT_FILL`), and only where clang is available.
//!
//! The objects are placed before anything is taken out, so one that has gone from the cache
//! since it was found is one more miss, and no build has to start over.

use std::collections::HashMap;
use std::path::PathBuf;

use velt_build_cache::{Cache, Key, KeyHasher};
use velt_vir::fingerprint::{Canon, Mode};
use velt_vir::vir;

use super::{compile, key, select, statics, stats, take_out, Place, KIND};
use crate::driver::BuildOptions;

/// `VELT_STD_OBJECT_FILL`: compile misses with LLVM and store them (`velt cache warm`), where
/// clang is available.
pub(super) fn fill() -> bool {
    std::env::var_os("VELT_STD_OBJECT_FILL").is_some() && velt_codegen_llvm::find_clang().is_some()
}

/// Whether objects built for the target's default CPU run wherever this build's program does: the
/// program's CPU has everything the default requires.
fn default_cpu_runs(opts: &BuildOptions, target: &str) -> bool {
    let backend = crate::backend::Backend::Cranelift;
    let required = |cpu| backend.required_features(target, cpu);
    match (required(opts.cpu.as_deref()), required(None)) {
        (Ok(program), Ok(default)) => default & !program == 0,
        _ => false,
    }
}

/// Take what the cache holds out of `program` (lowered, not optimized): the objects to link.
pub(in crate::driver) fn serve(
    program: &mut vir::Program,
    opts: &BuildOptions,
    cg: &velt_codegen_cl::CodegenOptions,
    place: &Place,
) -> Vec<PathBuf> {
    let Some(cache) = crate::build_cache::cache() else {
        return vec![];
    };
    let fill = fill();
    if !(fill || cache.has_files(KIND)) || !default_cpu_runs(opts, &cg.target) {
        return vec![];
    }
    let helpers = velt_opt::debug_inlined(&program.funcs);
    let user_sites = select::user_sites(program);
    let rule = select::Rule::Debug {
        helpers: &helpers,
        thread_data: velt_codegen_cl::has_thread_local_data(&cg.target),
        user_sites: &user_sites,
    };
    let canon = Canon::new(program, Mode::Link);
    let names = statics::Names::new(program, &canon);
    if stats() {
        eprintln!(
            "velt: std cache: std statements by reason: {:?}",
            select::reasons(program, &names, rule)
        );
    }
    let inputs = key::Inputs::debug(&cg.target);
    let mut digests = Digests::default();
    let looked_up: Vec<(usize, Key)> = (select::candidates(program, &names, rule).into_iter())
        .map(|i| (i, digests.key(program, &canon, &inputs, i)))
        .collect();
    drop(canon);
    let mut found: Vec<(usize, PathBuf)> = vec![];
    let mut missing: Vec<(usize, Key)> = vec![];
    for (i, key) in looked_up {
        match cache.path_of(KIND, &key) {
            Some(path) => found.push((i, path)),
            None => missing.push((i, key)),
        }
    }
    let misses = missing.len();
    let mut compiled = 0;
    if fill {
        let optimized = velt_codegen_cl::CodegenOptions {
            target: cg.target.clone(),
            optimize: true,
            cpu: None,
        };
        let stored = compile_missing(program, &names, &optimized, cache, missing);
        compiled = stored.len();
        found.extend(stored);
    }
    place.clear();
    let mut objects = vec![];
    let mut taken = vec![];
    for (i, path) in found {
        if let Ok(object) = place.put(objects.len(), &path) {
            objects.push(object);
            taken.push(i);
        }
    }
    taken.sort_unstable();
    for &i in &taken {
        let reached = statics::reached(program, &select::inlinable(program, i));
        statics::export(program, &names, &reached);
    }
    take_out(program, &taken);
    if stats() {
        eprintln!(
            "velt: std cache: {} found, {compiled} compiled, {} left in the program",
            taken.len() - compiled,
            misses - compiled
        );
    }
    objects
}

/// Compile `missing` on their own for `cg` and store them: where each stored one is. One that
/// can't be compiled alone stays in the program.
pub(super) fn compile_missing(
    program: &vir::Program,
    names: &statics::Names,
    cg: &velt_codegen_cl::CodegenOptions,
    cache: &Cache,
    missing: Vec<(usize, Key)>,
) -> Vec<(usize, PathBuf)> {
    let programs = (missing.iter())
        .map(|&(i, _)| compile::program_of(program, i, names))
        .collect();
    let objects = compile::each(programs, cg);
    (missing.into_iter().zip(objects))
        .filter_map(|((i, key), object)| {
            let path = compile::store(cache, &key, &object.ok()?)?;
            Some((i, path))
        })
        .collect()
}

/// What each function adds to a key, worked out once: a function is in the keys of everything
/// that inlines it, and a debug build asks for the key of every std function it reaches.
#[derive(Default)]
struct Digests {
    code: HashMap<usize, Key>,
    declared: HashMap<usize, Key>,
}

impl Digests {
    /// `key::Inputs::key` of function `i` (`select::inlinable` and `select::declared`).
    fn key(
        &mut self,
        program: &vir::Program,
        canon: &Canon,
        inputs: &key::Inputs,
        i: usize,
    ) -> Key {
        let set = select::inlinable(program, i);
        let declared = select::declared(program, &set);
        let code = set.iter().map(|&j| {
            *(self.code)
                .entry(j)
                .or_insert_with(|| digest(&key::Code::of(&program.funcs[j], canon)))
        });
        let mut parts: Vec<Key> = code.collect();
        parts.extend(declared.iter().map(|&j| {
            *(self.declared)
                .entry(j)
                .or_insert_with(|| digest(&key::Code::declared(&program.funcs[j])))
        }));
        inputs.key_of_digests(&parts)
    }
}

fn digest(code: &key::Code) -> Key {
    let mut h = KeyHasher::new();
    h.part(code.symbol.as_bytes());
    h.part(code.text.as_bytes());
    h.part(code.attrs.as_bytes());
    h.finish()
}
