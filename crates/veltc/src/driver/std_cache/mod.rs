//! The std cache (`VELT_STD_OBJECT=1`, release builds; #1728 step 3): std's functions compiled
//! once per variant, stored in the build cache, and linked into every program that compiles the
//! same code (design/std-object.md "The std cache").
//!
//! - After lowering, every std function that may be served (`select`) gets a key (`key`): its
//!   canonical code and parameter attributes, those of every small function it may inline, and
//!   who compiles it for what.
//! - Before the program is optimized ([`split`]), each such function becomes a declaration
//!   (`Import`), and the program's functions its object may call stay visible to it (`Export`).
//!   A key in the build cache (kind `stdfn`, a file entry) is a hit; for a miss, a program of
//!   the function's own is made then (`compile`).
//! - After ([`link`]), the program links the objects of those it still calls (the optimizer
//!   drops a declaration nothing calls any more, as it would have dropped the function); the
//!   misses among them are compiled now, with unknown callers, and stored under their keys.
//!
//! Code with a key is the same in every program that makes that key, so a hit links what this
//! program would have compiled itself, optimized without its callers' facts. If a function
//! cannot be compiled on its own, the build optimizes the program as it was before `split`, with
//! no std objects: the cache costs speed, never correctness.

mod compile;
mod key;
mod select;

use std::collections::HashSet;
use std::path::PathBuf;

use velt_build_cache::Key;
use velt_vir::fingerprint::{Canon, Mode};
use velt_vir::vir::{self, Linkage};

use super::BuildOptions;

/// The build cache's kind for std functions.
const KIND: &str = "stdfn";

/// Whether the std cache is on: release builds with `VELT_STD_OBJECT=1`.
pub(super) fn wanted(opts: &BuildOptions) -> bool {
    opts.release && std::env::var("VELT_STD_OBJECT").is_ok_and(|v| v == "1")
}

/// `VELT_STD_OBJECT_STATS`: say what the std cache did, and why std code stayed in the program.
pub(super) fn stats() -> bool {
    std::env::var_os("VELT_STD_OBJECT_STATS").is_some()
}

/// What the program links from the std cache, decided before it is optimized ([`split`]) and
/// done after ([`link`]).
pub(super) struct Plan {
    entries: Vec<Entry>,
}

struct Entry {
    symbol: String,
    object: Object,
    /// The other functions taken out that its object may call (directly, or through a small
    /// function compiled into it): linked whenever it is.
    calls: Vec<String>,
}

enum Object {
    /// In the cache.
    Found(PathBuf),
    /// Not yet: the function's program of its own, to compile if the program still calls it.
    Compile(Key, vir::Program),
}

/// What [`link`] did.
#[derive(Default)]
pub(super) struct Linked {
    /// The objects of the std functions taken out of the program, for the link.
    pub objects: Vec<PathBuf>,
    /// Functions found in the cache, compiled now, and taken out but no longer called (after
    /// optimization, as the program would have dropped them).
    pub hits: usize,
    pub misses: usize,
    pub dropped: usize,
}

/// Take the std functions the cache serves out of `program` (lowered, not optimized): each
/// becomes a declaration. For those not in the cache, their programs of their own are made now,
/// from this program; [`link`] compiles them once the program is optimized.
pub(super) fn split(
    program: &mut vir::Program,
    opts: &BuildOptions,
    cg: &velt_codegen_cl::CodegenOptions,
) -> Plan {
    let mut plan = Plan { entries: vec![] };
    let Some(cache) = crate::build_cache::cache() else {
        return plan;
    };
    if stats() {
        eprintln!(
            "velt: std cache: std statements by reason: {:?}",
            select::reasons(program)
        );
    }
    let candidates = select::candidates(program);
    if candidates.is_empty() {
        return plan;
    }
    let inputs = key::Inputs::of(opts, cg);
    let canon = Canon::new(program, Mode::Link);
    let taken: HashSet<usize> = candidates.iter().copied().collect();
    for &i in &candidates {
        let code = select::inlinable(program, i)
            .into_iter()
            .map(|j| key::Code::of(&program.funcs[j], &canon));
        let key = inputs.key(code);
        let object = match cache.path_of(KIND, &key) {
            Some(path) => Object::Found(path),
            None => Object::Compile(key, compile::program_of(program, i)),
        };
        let symbol = program.funcs[i].symbol.clone();
        let mut calls: Vec<String> = (select::inlinable(program, i).into_iter())
            .flat_map(|j| select::referenced(&program.funcs[j]))
            .filter(|c| *c != i && taken.contains(c))
            .map(|c| program.funcs[c].symbol.clone())
            .collect();
        calls.sort();
        calls.dedup();
        plan.entries.push(Entry {
            symbol,
            object,
            calls,
        });
    }
    drop(canon);
    take_out(program, &candidates);
    plan
}

/// After `program` is optimized: link what it still calls of what [`split`] took out, compiling
/// (in parallel) and storing the functions not yet in the cache. `Err` when one of them could
/// not be compiled on its own: the build then optimizes the program as it was before `split`.
pub(super) fn link(
    plan: Plan,
    program: &vir::Program,
    cg: &velt_codegen_cl::CodegenOptions,
) -> Result<Linked, String> {
    let mut out = Linked::default();
    let Some(cache) = crate::build_cache::cache() else {
        return Ok(out);
    };
    // What the program still calls, and what those objects call in turn.
    let mut needed: HashSet<&str> = (program.funcs.iter())
        .filter(|f| f.linkage == Linkage::Import)
        .map(|f| f.symbol.as_str())
        .collect();
    let by_symbol: std::collections::HashMap<&str, &Entry> = plan
        .entries
        .iter()
        .map(|e| (e.symbol.as_str(), e))
        .collect();
    let mut pending: Vec<&str> = needed.iter().copied().collect();
    while let Some(s) = pending.pop() {
        for c in by_symbol.get(s).map_or(&[][..], |e| &e.calls[..]) {
            if needed.insert(c.as_str()) {
                pending.push(c.as_str());
            }
        }
    }
    let called: HashSet<String> = needed.into_iter().map(str::to_string).collect();
    drop(by_symbol);
    let mut compile = vec![];
    for entry in plan.entries {
        if !called.contains(entry.symbol.as_str()) {
            out.dropped += 1;
            continue;
        }
        match entry.object {
            Object::Found(path) => {
                out.objects.push(path);
                out.hits += 1;
            }
            Object::Compile(key, alone) => compile.push((entry.symbol, key, alone)),
        }
    }
    let compiled = compile::each(compile.iter().map(|(_, _, p)| p).collect(), cg);
    for ((symbol, key, _), object) in compile.iter().zip(compiled) {
        let object = object.map_err(|e| format!("{symbol}: {e}"))?;
        let path = compile::store(cache, key, &object)
            .ok_or_else(|| format!("{symbol}: its object could not be stored"))?;
        out.objects.push(path);
        out.misses += 1;
    }
    Ok(out)
}

/// Make the functions `linked` declarations, and every function their objects may refer to (what
/// they refer to, and what the small functions they may inline refer to) visible to them.
fn take_out(program: &mut vir::Program, linked: &[usize]) {
    let linked_set: HashSet<usize> = linked.iter().copied().collect();
    let mut pending: Vec<usize> = linked.to_vec();
    let mut seen = linked_set.clone();
    while let Some(i) = pending.pop() {
        for c in select::referenced(&program.funcs[i]) {
            if !seen.insert(c) {
                continue;
            }
            let f = &mut program.funcs[c];
            if f.linkage == Linkage::Internal {
                f.linkage = Linkage::Export;
            }
            if select::small(f) {
                pending.push(c);
            }
        }
    }
    for &i in linked {
        let f = &mut program.funcs[i];
        if stats() {
            let size: usize = f.blocks.iter().map(|b| b.stmts.len() + 1).sum();
            eprintln!("velt: std cache: links {} ({size} statements)", f.symbol);
        }
        f.blocks.clear();
        f.locs.clear();
        f.locals.truncate(f.params.len());
        f.linkage = Linkage::Import;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_vir::vir::{BasicBlock, Callee, FuncId, Function, Local, Operand, Place, Terminator};

    fn calling(name: &str, callee: Option<u32>, stmts: usize) -> Function {
        let mut f = Function::new(name.into(), vec![], vir::Ty::I64, Linkage::Internal);
        f.locals.push(vir::LocalDecl::new(vir::Ty::I64, None));
        let nop = || vir::Stmt::Nop;
        let mut blocks = vec![];
        if let Some(c) = callee {
            blocks.push(BasicBlock {
                stmts: (0..stmts).map(|_| nop()).collect(),
                term: Terminator::Call {
                    callee: Callee::Func(FuncId(c)),
                    args: vec![],
                    dest: Some(Place::local(Local(0))),
                    next: vir::BlockId(1),
                },
            });
        }
        blocks.push(BasicBlock {
            stmts: vec![],
            term: Terminator::Return(Operand::Copy(Place::local(Local(0)))),
        });
        f.blocks = blocks;
        f
    }

    /// What a linked function's object may call back stays in the program, visible to it: what
    /// it calls, and what the small functions it may have inlined call. The function itself
    /// becomes a declaration.
    #[test]
    fn what_linked_code_may_call_is_exported() {
        let mut p = vir::Program {
            funcs: vec![
                calling("std_big", Some(1), 60),
                calling("small", Some(2), 1),
                calling("behind_small", None, 0),
                calling("unrelated", None, 0),
            ],
            ..vir::Program::default()
        };
        take_out(&mut p, &[0]);
        let linkage: Vec<Linkage> = p.funcs.iter().map(|f| f.linkage).collect();
        assert_eq!(
            linkage,
            [
                Linkage::Import,
                Linkage::Export,
                Linkage::Export,
                Linkage::Internal
            ]
        );
        assert!(p.funcs[0].blocks.is_empty() && p.funcs[0].locals.is_empty());
    }
}
