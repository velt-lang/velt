//! One call per function of each function that never returns (`cold_calls`).
//!
//! Every checked array access, division and throw helper call leaves a call that never
//! returns behind (`_Goob_u64(len, i, at)`, `velt_rt_panic(msg)`, `corrupt("…")`), one per
//! check, each with its own constants. In a small reader such as `readU32(data, at)` those calls
//! are most of the code: LLVM's inliner counts every call (and its arguments) at full price
//! whether or not it can ever run, so four bounds checks put a four-byte read over the
//! threshold for being inlined into its callers, and a function that was inlined into its
//! caller by `velt_opt` carries its calls into it.
//!
//! The calls of one function to the same callee become one: each call site assigns the
//! arguments that differ between the sites to locals (the same for every site) and jumps to a
//! single call block, which passes those locals (an argument every site passes the same stays
//! a constant). The backends turn the locals into phis. A call never returns, so nothing after
//! it is affected; its location is the first site's (panic messages carry their own location).
//!
//! A callee counts when it never returns: a `noreturn` extern, or a function no call of which
//! returns normally (`inline::never_returns`), whose call block's successor is `Unreachable`
//! and that writes no result.

#[cfg(test)]
mod tests;

use rustc_hash::FxHashMap;

use velt_vir::vir::{
    BasicBlock, BlockId, Callee, Function, LocalDecl, Operand, Place, Program, Rvalue, Stmt,
    Terminator, Ty,
};

use crate::srclocs::push_stmt;

/// The callees that never return, with their parameter types.
pub(crate) struct NoReturn {
    funcs: Vec<Option<Vec<Ty>>>,
    externs: Vec<Option<Vec<Ty>>>,
}

impl NoReturn {
    pub fn find(program: &Program) -> NoReturn {
        let funcs = (program.funcs.iter())
            .map(|f| crate::inline::never_returns(f).then(|| f.params.clone()))
            .collect();
        let externs = (program.externs.iter())
            .map(|e| e.noreturn.then(|| e.params.clone()))
            .collect();
        NoReturn { funcs, externs }
    }

    fn params(&self, callee: &Callee) -> Option<&[Ty]> {
        match callee {
            Callee::Func(f) => self.funcs.get(f.0 as usize)?.as_deref(),
            Callee::Extern(e) => self.externs.get(e.0 as usize)?.as_deref(),
            Callee::Ptr { .. } => None,
        }
    }
}

/// The callee of a call site.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Func(u32),
    Extern(u32),
}

/// Merge `func`'s calls of each never-returning callee into one; returns whether any changed.
pub(crate) fn run(no_return: &NoReturn, func: &mut Function) -> bool {
    let mut sites: FxHashMap<Key, Vec<usize>> = FxHashMap::default();
    let mut order = Vec::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        let Terminator::Call {
            callee,
            args,
            dest: None,
            next,
        } = &block.term
        else {
            continue;
        };
        let key = match callee {
            Callee::Func(f) => Key::Func(f.0),
            Callee::Extern(e) => Key::Extern(e.0),
            Callee::Ptr { .. } => continue,
        };
        let after = &func.blocks[next.0 as usize];
        let dead_end = after.stmts.is_empty() && matches!(after.term, Terminator::Unreachable);
        let fits = no_return
            .params(callee)
            .is_some_and(|p| p.len() == args.len());
        if dead_end && fits {
            let list = sites.entry(key).or_default();
            if list.is_empty() {
                order.push(key);
            }
            list.push(bi);
        }
    }
    let mut changed = false;
    for key in order {
        let blocks = &sites[&key];
        if blocks.len() > 1 {
            merge(no_return, func, blocks);
            changed = true;
        }
    }
    changed
}

/// The arguments of the call ending block `bi`, and the rest of its terminator.
fn call_of(func: &Function, bi: usize) -> (&Callee, &[Operand], BlockId) {
    match &func.blocks[bi].term {
        Terminator::Call {
            callee, args, next, ..
        } => (callee, args, *next),
        _ => panic!("ICE: cold_calls: a site is not a call"),
    }
}

/// Make the calls ending `blocks` (same callee) one call.
fn merge(no_return: &NoReturn, func: &mut Function, blocks: &[usize]) {
    let (callee, first_args, next) = call_of(func, blocks[0]);
    let callee = callee.clone();
    let first_args = first_args.to_vec();
    let params = (no_return.params(&callee))
        .unwrap_or_else(|| panic!("ICE: cold_calls: callee returns"))
        .to_vec();
    // Per argument: the local every site assigns, or `None` when every site passes the same.
    let mut slots = Vec::with_capacity(first_args.len());
    for (k, a) in first_args.iter().enumerate() {
        let same = (blocks.iter()).all(|&bi| call_of(func, bi).1[k] == *a);
        slots.push((!same).then(|| {
            func.locals.push(LocalDecl::new(params[k], None));
            velt_vir::vir::Local(func.locals.len() as u32 - 1)
        }));
    }
    let args = (first_args.iter().zip(&slots))
        .map(|(a, s)| match s {
            Some(l) => Operand::Copy(Place::local(*l)),
            None => a.clone(),
        })
        .collect();
    let first_loc = func.term_loc(blocks[0]);
    let merged = BlockId(func.blocks.len() as u32);
    func.blocks.push(BasicBlock {
        stmts: Vec::new(),
        term: Terminator::Call {
            callee,
            args,
            dest: None,
            next,
        },
    });
    if !func.locs.is_empty() {
        func.locs.push(vec![first_loc]);
    }
    for &bi in blocks {
        let at = func.term_loc(bi);
        let term = std::mem::replace(&mut func.blocks[bi].term, Terminator::Goto(merged));
        let Terminator::Call { args, .. } = term else {
            panic!("ICE: cold_calls: a site is not a call");
        };
        for (a, s) in args.into_iter().zip(&slots) {
            if let Some(l) = s {
                push_stmt(func, bi, Stmt::Assign(Place::local(*l), Rvalue::Use(a)), at);
            }
        }
    }
}
