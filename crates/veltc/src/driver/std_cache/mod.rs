//! The std cache (`VELT_STD_OBJECT=1`, release builds; #1728 step 3): std's functions compiled
//! once per variant, stored in the build cache, and linked into every program that compiles the
//! same code (design/std-object.md "The std cache").
//!
//! - After lowering, every std function that may be served (`select`) gets a key (`key`): its
//!   canonical code and parameter attributes, those of every small function it may inline, and
//!   who compiles it for what.
//! - A key in the build cache (kind `stdfn`, a file entry) is a hit. A miss is compiled now, on
//!   its own (`compile`): with unknown callers, in a copy of the program where every other
//!   function is a declaration, small ones keeping their bodies to be inlined. Its object is
//!   stored under its key.
//! - Either way the function becomes a declaration (`Import`) in the program and its object goes
//!   to the link; the program's functions the object may call stay visible to it (`Export`).
//!
//! Code with a key is the same in every program that makes that key, so a hit links what this
//! program would have compiled itself, optimized without its callers' facts. A function that
//! could not be compiled on its own stays in the program: the cache costs speed, never
//! correctness.

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

/// What [`split`] did.
#[derive(Default)]
pub(super) struct Split {
    /// The objects of the std functions taken out of the program, for the link.
    pub objects: Vec<PathBuf>,
    /// Functions found in the cache, and compiled now.
    pub hits: usize,
    pub misses: usize,
}

/// Take the std functions the cache serves out of `program` (lowered, not optimized): each
/// becomes a declaration, and its object (from the cache, or compiled now) is returned.
pub(super) fn split(
    program: &mut vir::Program,
    opts: &BuildOptions,
    cg: &velt_codegen_cl::CodegenOptions,
) -> Split {
    let mut out = Split::default();
    let Some(cache) = crate::build_cache::cache() else {
        return out;
    };
    if stats() {
        eprintln!(
            "velt: std cache: std statements by reason: {:?}",
            select::reasons(program)
        );
    }
    let candidates = select::candidates(program);
    if candidates.is_empty() {
        return out;
    }
    let inputs = key::Inputs::of(opts, cg);
    let canon = Canon::new(program, Mode::Link);
    let keys: Vec<(usize, Key)> = (candidates.iter())
        .map(|&i| {
            (
                i,
                inputs.key(select::inlinable(program, i).iter().map(|&j| {
                    let f = &program.funcs[j];
                    key::Code::of(f, &canon)
                })),
            )
        })
        .collect();
    drop(canon);
    let mut linked: Vec<usize> = vec![];
    let mut misses = vec![];
    for &(i, key) in &keys {
        match cache.path_of(KIND, &key) {
            Some(path) => {
                out.objects.push(path);
                out.hits += 1;
                linked.push(i);
            }
            None => misses.push((i, key)),
        }
    }
    if !misses.is_empty() {
        let compiled = compile::each(program, &misses, cg);
        for ((i, key), object) in misses.iter().zip(compiled) {
            let Some(path) = object.and_then(|o| compile::store(cache, key, &o)) else {
                // Not compiled or not stored: it stays in the program.
                continue;
            };
            out.objects.push(path);
            out.misses += 1;
            linked.push(*i);
        }
    }
    take_out(program, &linked);
    out
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
            eprintln!("velt: std cache: links {}", f.symbol);
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
