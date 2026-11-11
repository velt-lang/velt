//! Velt functions that only read the memory their callers can see (#2198): a call of one keeps a
//! borrowed copy as a read-only runtime call does (`Runtime::reads_only`), so `f(lines[k])`
//! hands `f` the element itself rather than a clone it drops after the call.
//!
//! A function qualifies when its body:
//! - writes only its own locals: no assignment through a pointer (`Proj::Deref`), no `MemCopy`,
//!   `MemCopyDyn` or `MemSet` (their destination is a pointer), and every call's result goes to a
//!   local;
//! - calls only runtime functions that write nothing compiled code reads
//!   (`Effects::writes_nothing_visible`), runtime functions that never return (a panic: the
//!   caller's copy is not used after it), and functions that qualify themselves.
//!
//! A function without a body here (imported from another unit), one that returns a pointer
//! (which may point into what it was handed, letting the borrow escape) and any indirect call do
//! not qualify. Freeing writes (the allocator's state and the freed memory), so a function that frees
//! anything calls a runtime function that is not read-only and does not qualify either.
//!
//! The set is the greatest fixed point: every function with a body starts in it and leaves when
//! its body breaks a rule, given the set so far, until nothing leaves. Recursion therefore
//! qualifies when nothing else in the cycle breaks a rule.

use rustc_hash::FxHashSet;
use velt_vir::vir::{
    Callee, ExternId, FuncId, Function, Linkage, Place, Program, Proj, Stmt, Terminator, Ty,
};

/// The functions of `program` that qualify, given the runtime functions that write nothing
/// visible (`read_only`).
pub(crate) fn read_only_functions(
    program: &Program,
    read_only: &FxHashSet<ExternId>,
) -> FxHashSet<FuncId> {
    let mut set: FxHashSet<FuncId> = (program.funcs.iter().enumerate())
        .filter(|(_, f)| has_body(f))
        .map(|(i, _)| FuncId(i as u32))
        .collect();
    loop {
        let leaving: Vec<FuncId> = (set.iter().copied())
            .filter(|&f| !qualifies(program, &program.funcs[f.0 as usize], read_only, &set))
            .collect();
        if leaving.is_empty() {
            return set;
        }
        for f in leaving {
            set.remove(&f);
        }
    }
}

fn has_body(f: &Function) -> bool {
    f.linkage != Linkage::Import && !f.blocks.is_empty() && f.ret != Ty::Ptr
}

/// Is `place` a local or a part of one (no pointer followed)?
fn is_local(place: &Place) -> bool {
    !place.proj.iter().any(|p| matches!(p, Proj::Deref(_)))
}

fn qualifies(
    program: &Program,
    func: &Function,
    read_only: &FxHashSet<ExternId>,
    set: &FxHashSet<FuncId>,
) -> bool {
    func.blocks.iter().all(|block| {
        let stmts_ok = block.stmts.iter().all(|s| match s {
            Stmt::Assign(place, _) => is_local(place),
            Stmt::Nop => true,
            Stmt::MemCopy { .. } | Stmt::MemCopyDyn { .. } | Stmt::MemSet { .. } => false,
        });
        stmts_ok
            && match &block.term {
                Terminator::Call { callee, dest, .. } => {
                    dest.as_ref().is_none_or(is_local)
                        && match callee {
                            Callee::Extern(e) => {
                                read_only.contains(e) || program.externs[e.0 as usize].noreturn
                            }
                            Callee::Func(f) => set.contains(f),
                            Callee::Ptr { .. } => false,
                        }
                }
                _ => true,
            }
    })
}
