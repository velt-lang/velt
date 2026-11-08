//! A release build's default use of the std cache: the std functions marked `noInline()` link
//! objects compiled once, optimized as a release build optimizes them, and everything else goes on
//! as in any release build.
//!
//! The optimizer never inlines a `noInline()` function into a caller, so its object holds the code
//! the program would have compiled for it, and the program's clang no longer compiles it. Every
//! other std function stays in the program, where it can be inlined and specialized
//! (`select::Rule::NoInline`). The object is compiled with copies of its own of every function it
//! reaches through functions not marked `noInline()` (`select::Depth::Whole`, all in its key), so
//! velt_opt and LLVM optimize it with the callees the program would optimize it with; only the
//! `noInline()` functions it calls, and those whose address it takes, are the program's. What it
//! can't see is its callers' arguments (constants the program could fold into it).
//!
//! As in a debug build (`serve`): a function whose key is in the cache becomes a declaration, its
//! object placed before it comes out; a miss stays in the program, and no clang starts for it.
//! Only `velt cache warm` stores release objects (`VELT_STD_OBJECT_FILL`). After optimization the
//! program links the objects of what it still calls ([`Served::linked`]), as [`super::link`] does.
//! The keys are [`super::split`]'s: the CPU the program is built for, clang, and a release build's
//! code, so a debug build's object never serves a release build.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use velt_vir::fingerprint::{Canon, Mode};
use velt_vir::vir::{self, Linkage};

use super::{key, select, serve, statics, stats, take_out, Place, KIND};

/// What a release build's objects are compiled with: copies of their own of every function they
/// reach through functions not marked `noInline()` (`select::Depth::Whole`).
const DEPTH: select::Depth = select::Depth::Whole;
use crate::driver::BuildOptions;

/// What [`serve`] took out of the program: per function, its symbol, the other functions taken
/// out that its object may call, and its object in place.
#[derive(Default)]
pub(in crate::driver) struct Served {
    entries: Vec<(String, Vec<String>, PathBuf)>,
}

/// Whether a release build of `program` looks in the cache: there is one, it has held objects
/// (or this build stores them), and `program` has a std function marked `noInline()`. Without
/// them a build does nothing it would not do without the cache.
pub(in crate::driver) fn applies(program: &vir::Program) -> bool {
    let Some(cache) = crate::build_cache::cache() else {
        return false;
    };
    (cache.has_files(KIND) || serve::fill()) && select::has_no_inline(program)
}

/// Take the `noInline()` std functions the cache holds out of `program` (lowered, not optimized),
/// placing their objects in `place`; with `VELT_STD_OBJECT_FILL`, compile the others for `cg`
/// and store them first.
pub(in crate::driver) fn serve(
    program: &mut vir::Program,
    opts: &BuildOptions,
    cg: &velt_codegen_cl::CodegenOptions,
    place: &Place,
) -> Served {
    let Some(cache) = crate::build_cache::cache() else {
        return Served::default();
    };
    let simd = select::simd_checking(program);
    let rule = select::Rule::NoInline { simd: &simd };
    let canon = Canon::new(program, Mode::Link);
    let names = statics::Names::new(program, &canon);
    if stats() {
        eprintln!(
            "velt: std cache: std statements by reason: {:?}",
            select::reasons(program, &names, rule)
        );
    }
    let inputs = key::Inputs::of(opts, cg);
    let mut found: Vec<(usize, PathBuf)> = vec![];
    let mut missing = vec![];
    for i in select::candidates(program, &names, rule) {
        let key = super::release_key(program, &canon, &inputs, i, DEPTH);
        match cache.path_of(KIND, &key) {
            Some(path) => found.push((i, path)),
            None => missing.push((i, key)),
        }
    }
    drop(canon);
    let misses = missing.len();
    let mut compiled = 0;
    if serve::fill() {
        let stored = serve::compile_missing(program, &names, cg, cache, missing, DEPTH);
        compiled = stored.len();
        found.extend(stored);
    }
    place.clear();
    let mut placed: Vec<(usize, PathBuf)> = vec![];
    for (i, path) in found {
        if let Ok(object) = place.put(placed.len(), &path) {
            placed.push((i, object));
        }
    }
    placed.sort_unstable_by_key(|&(i, _)| i);
    let taken: HashSet<usize> = placed.iter().map(|&(i, _)| i).collect();
    // Every set as keyed, before anything is exported: `inlinable` follows internal functions
    // only, and exporting one would hide it from the sets after.
    let linked: Vec<(usize, Vec<usize>)> = (placed.iter())
        .map(|&(i, _)| (i, select::inlinable(program, i, DEPTH)))
        .collect();
    let mut served = Served::default();
    for ((i, object), (_, set)) in placed.into_iter().zip(&linked) {
        let mut calls: Vec<String> = (set.iter())
            .flat_map(|&j| select::referenced(&program.funcs[j]))
            .filter(|c| *c != i && taken.contains(c))
            .map(|c| program.funcs[c].symbol.clone())
            .collect();
        calls.sort();
        calls.dedup();
        let symbol = program.funcs[i].symbol.clone();
        served.entries.push((symbol, calls, object));
    }
    for (_, set) in &linked {
        let reached = statics::reached(program, set);
        statics::export(program, &names, &reached);
    }
    take_out(program, &linked, DEPTH);
    if stats() {
        eprintln!(
            "velt: std cache: {} found, {compiled} compiled, {} left in the program",
            linked.len().saturating_sub(compiled),
            misses - compiled
        );
    }
    served
}

impl Served {
    /// After `program` is optimized: the objects of the functions it still calls, closed over
    /// what those objects call. The others are removed from their place, as the optimizer dropped
    /// their declarations (it would have dropped the functions).
    pub fn linked(self, program: &vir::Program) -> Vec<PathBuf> {
        let calls: HashMap<&str, &[String]> = (self.entries.iter())
            .map(|(symbol, calls, _)| (symbol.as_str(), &calls[..]))
            .collect();
        let mut needed: HashSet<&str> = (program.funcs.iter())
            .filter(|f| f.linkage == Linkage::Import && calls.contains_key(f.symbol.as_str()))
            .map(|f| f.symbol.as_str())
            .collect();
        let mut pending: Vec<&str> = needed.iter().copied().collect();
        while let Some(s) = pending.pop() {
            for c in calls.get(s).copied().unwrap_or_default() {
                if needed.insert(c.as_str()) {
                    pending.push(c.as_str());
                }
            }
        }
        let mut objects = vec![];
        let mut dropped = 0;
        for (symbol, _, object) in &self.entries {
            match needed.contains(symbol.as_str()) {
                true => objects.push(object.clone()),
                false => {
                    let _ = std::fs::remove_file(object);
                    dropped += 1;
                }
            }
        }
        if stats() {
            eprintln!("velt: std cache: {dropped} no longer called");
        }
        objects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// After optimization, the objects linked are those of the functions still declared and of
    /// what their objects call; the others leave their place.
    #[test]
    fn only_what_is_still_called_is_linked() {
        let tmp = tempfile::tempdir().unwrap();
        let object = |name: &str| {
            let path = tmp.path().join(name);
            std::fs::write(&path, b"object").unwrap();
            path
        };
        let served = Served {
            entries: vec![
                ("a".into(), vec!["b".into()], object("0.o")),
                ("b".into(), vec![], object("1.o")),
                ("c".into(), vec![], object("2.o")),
            ],
        };
        let declared =
            |s: &str| vir::Function::new(s.into(), vec![], vir::Ty::I64, Linkage::Import);
        let program = vir::Program {
            funcs: vec![declared("a"), declared("velt_rt_other")],
            ..vir::Program::default()
        };
        let linked = served.linked(&program);
        assert_eq!(linked, [tmp.path().join("0.o"), tmp.path().join("1.o")]);
        assert!(!tmp.path().join("2.o").exists(), "no longer called");
    }
}
