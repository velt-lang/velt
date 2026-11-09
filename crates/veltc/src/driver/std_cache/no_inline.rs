//! A release build's default use of the std cache: the std functions marked `@noInline` link
//! objects compiled once, optimized as a release build optimizes them, and everything else goes on
//! as in any release build.
//!
//! The optimizer never inlines a `@noInline` function into a caller, so its object holds the code
//! the program would have compiled for it, and the program's clang no longer compiles it. Every
//! other std function stays in the program, where it can be inlined and specialized
//! (`select::Rule::NoInline`). The object is compiled with copies of its own of every function it
//! reaches through functions not marked `@noInline` (`select::Depth::Whole`, all in its key), so
//! velt_opt and LLVM optimize it with the callees the program would optimize it with; only the
//! `@noInline` functions it calls, and those whose address it takes, are the program's. What it
//! can't see is its callers' arguments (constants the program could fold into it).
//!
//! As in a debug build (`serve`), a function whose key is in the cache is served, its object placed
//! before the program is optimized; a miss stays in the program, and no clang starts for it. Only
//! `velt cache warm` stores release objects (`VELT_STD_OBJECT_FILL`). Unlike a debug build's, a
//! served function stays in the program while velt_opt optimizes it, and becomes a declaration
//! after ([`Served::take_out`]): it is never inlined, but what it spends of the inliner's budget,
//! inlining its callees into itself, and the room it takes in that budget, would otherwise go to
//! the rest of the program, which would be inlined differently for it (`bench/stdport/brotli_stream_*`
//! +3.5 to +6.6 % with it taken out before). So the program's code is the same as without the
//! cache, but for the functions made visible to the objects. The functions its object calls are made visible to it (`Export`) before, so that the
//! optimizer takes no facts about them from the program's calls. Then the program links the objects
//! of what it still calls ([`Served::linked`]), as [`super::link`] does.
//! The keys are [`super::split`]'s: the CPU the program is built for, clang, and a release build's
//! code, so a debug build's object never serves a release build.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use velt_vir::fingerprint::{Canon, Mode};
use velt_vir::vir::{self, Linkage};

use super::{declare, expose, key, select, serve, statics, stats, Place, KIND};
use crate::driver::BuildOptions;

/// What a release build's objects are compiled with: copies of their own of every function they
/// reach through functions not marked `@noInline` (`select::Depth::Whole`).
const DEPTH: select::Depth = select::Depth::Whole;

/// What [`serve`] serves: per function, its symbol, the other functions served that its object
/// may call, and its object in place. [`Served::take_out`] takes them out once the program is
/// optimized.
#[derive(Default)]
pub(in crate::driver) struct Served {
    entries: Vec<(String, Vec<String>, PathBuf)>,
}

impl Served {
    /// Whether nothing is served.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Whether a release build of `program` looks in the cache: there is one, it has held objects
/// (or this build stores them), and `program` has a std function marked `@noInline`. Without
/// them a build does nothing it would not do without the cache.
pub(in crate::driver) fn applies(program: &vir::Program) -> bool {
    let Some(cache) = crate::build_cache::cache() else {
        return false;
    };
    (cache.has_files(KIND) || serve::fill()) && select::has_no_inline(program)
}

/// Serve the `@noInline` std functions the cache holds (lowered, not optimized),
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
    let mut memo = HashMap::new();
    for i in select::candidates(program, &names, rule) {
        let key = super::release_key(program, &canon, &inputs, i, DEPTH, &mut memo);
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
    expose(program, &linked, DEPTH);
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
    /// After `program` is optimized: each served function it still has a declaration, and the
    /// functions only they called removed (the optimizer kept them for them).
    pub fn take_out(&self, program: &mut vir::Program) {
        if self.entries.is_empty() {
            return;
        }
        let served: HashSet<&str> = (self.entries.iter()).map(|(s, _, _)| s.as_str()).collect();
        for f in &mut program.funcs {
            if served.contains(f.symbol.as_str()) && !f.blocks.is_empty() {
                declare(f);
            }
        }
        velt_opt::remove_unused_functions(program);
    }

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

    /// `name(x)`: `stmts` times `x = x * 3`, then `x = f(x)` for each `f` of `calls` (by index).
    fn func(name: &str, stmts: usize, calls: &[usize], linkage: Linkage) -> vir::Function {
        use vir::{BasicBlock, BlockId, Callee, Const, FuncId, Local, Operand, Place, Rvalue};
        let mut f = vir::Function::new(name.into(), vec![vir::Ty::I64], vir::Ty::I64, linkage);
        let times3 = |_| {
            vir::Stmt::Assign(
                Place::local(Local(0)),
                Rvalue::Binary(
                    vir::BinOp::Mul,
                    Operand::Copy(Place::local(Local(0))),
                    Operand::Const(Const::Int(3), vir::Ty::I64),
                ),
            )
        };
        let mut stmts: Vec<vir::Stmt> = (0..stmts).map(times3).collect();
        let mut blocks = vec![];
        for (n, &c) in calls.iter().enumerate() {
            blocks.push(BasicBlock {
                stmts: std::mem::take(&mut stmts),
                term: vir::Terminator::Call {
                    callee: Callee::Func(FuncId(c as u32)),
                    args: vec![Operand::Copy(Place::local(Local(0)))],
                    dest: Some(Place::local(Local(0))),
                    next: BlockId(n as u32 + 1),
                },
            });
        }
        blocks.push(BasicBlock {
            stmts,
            term: vir::Terminator::Return(Operand::Copy(Place::local(Local(0)))),
        });
        f.blocks = blocks;
        f
    }

    /// Every function but the served one, as text.
    fn rest(program: &vir::Program) -> Vec<String> {
        (program.funcs.iter())
            .filter(|f| f.symbol != "served" && f.symbol != "only_served")
            .map(|f| format!("{} {:?} {:?}", f.symbol, f.linkage, f.blocks))
            .collect()
    }

    /// The program's code is the same with a function served as without (the brotli shape, #2607):
    /// `served` (`@noInline`) inlines `helper` 20 times, which spends the inliner's budget before
    /// `entry` inlines `small` at as many of its 100 calls as the budget lasts. Taken out before
    /// optimization, `served` spent nothing and took no room in the budget, and `entry` inlined
    /// `small` at other sites; taken out after, the rest is as without the cache, and what only
    /// `served` called is removed.
    #[test]
    fn a_function_taken_out_after_optimization_leaves_the_rest_as_without_the_cache() {
        let served_calls: Vec<usize> = [1; 20].into_iter().chain([4]).collect();
        let mut served_fn = func("served", 0, &served_calls, Linkage::Internal);
        served_fn.inline = vir::Inline::Never;
        let entry_calls: Vec<usize> = std::iter::once(0).chain([2; 100]).collect();
        // Called by `served` alone and never inlined: in the program only for it. Last, so that
        // removing it renumbers nothing else.
        let mut only_served = func("only_served", 50, &[], Linkage::Internal);
        only_served.inline = vir::Inline::Never;
        let program = vir::Program {
            funcs: vec![
                served_fn,
                func("helper", 34, &[], Linkage::Internal),
                func("small", 34, &[], Linkage::Internal),
                func("entry", 0, &entry_calls, Linkage::Export),
                only_served,
            ],
            ..vir::Program::default()
        };
        let mut before = program.clone();
        declare(&mut before.funcs[0]);
        velt_opt::optimize(&mut before, velt_opt::OptLevel::Speed);
        let mut without = program.clone();
        velt_opt::optimize(&mut without, velt_opt::OptLevel::Speed);
        let served = Served {
            entries: vec![("served".into(), vec![], PathBuf::from("0.o"))],
        };
        let mut with = program;
        velt_opt::optimize(&mut with, velt_opt::OptLevel::Speed);
        served.take_out(&mut with);
        let calls = |p: &vir::Program| {
            let small = p.funcs.iter().position(|f| f.symbol == "small");
            let entry = p.funcs.iter().find(|f| f.symbol == "entry").unwrap();
            (entry.blocks.iter())
                .filter(|b| matches!(&b.term, vir::Terminator::Call { callee: vir::Callee::Func(c), .. } if Some(c.0 as usize) == small))
                .count()
        };
        // The budget limits the inlining: some of `small`'s sites are left.
        assert!((1..100).contains(&calls(&without)), "{}", calls(&without));
        assert_ne!(
            calls(&before),
            calls(&without),
            "taken out before, it moves the rest"
        );
        assert_eq!(calls(&with), calls(&without), "sites of `small` left");
        assert!(rest(&with) == rest(&without), "the rest's code differs");
        let declared = with.funcs.iter().find(|f| f.symbol == "served").unwrap();
        assert!(declared.blocks.is_empty() && declared.linkage == Linkage::Import);
        assert!(without.funcs.iter().any(|f| f.symbol == "only_served"));
        assert!(
            with.funcs.iter().all(|f| f.symbol != "only_served"),
            "what only it called is gone"
        );
    }
}
