//! Module state lookups once per function call (#1571).
//!
//! Every access to a module-level `let` asks the runtime for the module state block,
//! `velt_rt_module_state(&DESC, slot)` (rt_abi.md "Module state"): it takes the module lock unless
//! the thread holds it, finds the block and checks that `slot` is initialized. That is a call
//! with two thread-local lookups, opaque to the backends, at every read and write, so a loop over
//! module state does it each iteration.
//!
//! Within one call of a VIR function the answer cannot change once it has been given:
//! - the block of a descriptor stays the same while the program runs;
//! - the lock, once taken, is held until the poll ends, and a poll never ends inside a call of a
//!   VIR function: an async function or a generator is a poll / resume function that returns at
//!   each suspension, and the next poll that touched module state takes the lock at its start;
//! - a slot found initialized stays initialized.
//!
//! So this pass gives a function two locals per descriptor, `checked` (slots below it were
//! checked in this call; 0 on entry) and `block`, and turns each call into
//! `if checked <= slot { block = velt_rt_module_state(&DESC, slot); checked = slot + 1 }`
//! followed by a copy of `block`. The first access asks the runtime as before (an uninitialized
//! slot still stops with its `ReferenceError` at the same point); later ones, in loops too, are a
//! compare and a predictable branch. `velt_rt_block_on` hands the lock to the tasks, so a
//! function that calls it is left as it is. Runs once the rounds are done, so calls inlined into
//! one function share one lookup.

use velt_vir::vir::{
    BasicBlock, BinOp, BlockId, Callee, Const, ExternId, Function, Local, LocalDecl, Operand,
    Place, Program, Rvalue, Stmt, Terminator, Ty,
};

use crate::srclocs::{prepend_stmts, push_stmt};

/// The runtime functions this pass knows, by id in one program.
#[derive(Clone, Copy)]
pub(crate) struct Lookups {
    state: Option<ExternId>,
    block_on: Option<ExternId>,
}

impl Lookups {
    pub(crate) fn find(program: &Program) -> Lookups {
        let ext = |name: &str| {
            program
                .externs
                .iter()
                .position(|e| e.symbol == name)
                .map(|i| ExternId(i as u32))
        };
        Lookups {
            state: ext("velt_rt_module_state"),
            block_on: ext("velt_rt_block_on"),
        }
    }
}

/// One `velt_rt_module_state(desc, slot)` call: its block, descriptor, slot and destination.
struct Site {
    block: usize,
    desc: Const,
    slot: i128,
    dest: Place,
}

/// Cache the module state lookups of `func`; returns whether any call changed.
pub(crate) fn run(lookups: Lookups, func: &mut Function) -> bool {
    let Some(state) = lookups.state else {
        return false;
    };
    let mut sites = vec![];
    for (bi, b) in func.blocks.iter().enumerate() {
        let Terminator::Call {
            callee: Callee::Extern(e),
            args,
            dest,
            ..
        } = &b.term
        else {
            continue;
        };
        if Some(*e) == lookups.block_on {
            return false;
        }
        if *e != state {
            continue;
        }
        match (args.as_slice(), dest) {
            (
                [Operand::Const(desc @ Const::Static(_), Ty::Ptr), Operand::Const(Const::Int(slot), Ty::U64)],
                Some(dest),
            ) => sites.push(Site {
                block: bi,
                desc: desc.clone(),
                slot: *slot,
                dest: dest.clone(),
            }),
            _ => return false,
        }
    }
    if !worth_it(func, &sites) {
        return false;
    }
    // One `(checked, block)` pair per descriptor (one per program, but code swapped in by
    // `velt dev` has its own).
    let mut caches: Vec<(Const, Local, Local)> = vec![];
    for site in sites {
        let (checked, block) = match caches.iter().find(|c| c.0 == site.desc) {
            Some(&(_, c, b)) => (c, b),
            None => {
                let c = new_local(func, Ty::U64, "ms.checked");
                let b = new_local(func, Ty::Ptr, "ms.block");
                caches.push((site.desc.clone(), c, b));
                (c, b)
            }
        };
        rewrite(func, &site, checked, block);
    }
    let init = caches
        .iter()
        .flat_map(|&(_, c, b)| {
            [
                Stmt::Assign(Place::local(c), Rvalue::Use(int(0, Ty::U64))),
                Stmt::Assign(Place::local(b), Rvalue::Use(int(0, Ty::Ptr))),
            ]
        })
        .collect();
    prepend_stmts(func, 0, init);
    true
}

/// Two lookups, or one that may run more than once per call (in a loop).
fn worth_it(func: &Function, sites: &[Site]) -> bool {
    if sites.len() >= 2 {
        return true;
    }
    let Some(site) = sites.first() else {
        return false;
    };
    let edges: Vec<Vec<usize>> = func
        .blocks
        .iter()
        .map(|b| {
            crate::visit::successors(&b.term)
                .into_iter()
                .map(|s| s.0 as usize)
                .collect()
        })
        .collect();
    crate::scc::on_cycle(&edges)[site.block]
}

/// Block `site.block` ends in the lookup; it becomes
/// `need = checked <= slot; branch need, ask, done` with `ask: block = lookup(..) -> set`,
/// `set: checked = slot + 1 -> done` and `done: dest = block -> next`.
fn rewrite(func: &mut Function, site: &Site, checked: Local, block: Local) {
    let bi = site.block;
    let Terminator::Call {
        callee, args, next, ..
    } = std::mem::replace(&mut func.blocks[bi].term, Terminator::Unreachable)
    else {
        unreachable!("ICE: module state site is not a call")
    };
    let at = func.locs.get(bi).and_then(|l| l.last().copied().flatten());
    let need = new_local(func, Ty::Bool, "ms.need");
    let ask = add_block(
        func,
        vec![],
        Terminator::Call {
            callee,
            args,
            dest: Some(Place::local(block)),
            next: BlockId(func.blocks.len() as u32 + 1),
        },
        at,
    );
    let done_id = BlockId(func.blocks.len() as u32 + 1);
    add_block(
        func,
        vec![Stmt::Assign(
            Place::local(checked),
            Rvalue::Use(int(site.slot + 1, Ty::U64)),
        )],
        Terminator::Goto(done_id),
        None,
    );
    let done = add_block(
        func,
        vec![Stmt::Assign(
            site.dest.clone(),
            Rvalue::Use(Operand::Copy(Place::local(block))),
        )],
        Terminator::Goto(next),
        None,
    );
    debug_assert_eq!(done, done_id);
    push_stmt(
        func,
        bi,
        Stmt::Assign(
            Place::local(need),
            Rvalue::Binary(
                BinOp::Le,
                Operand::Copy(Place::local(checked)),
                int(site.slot, Ty::U64),
            ),
        ),
        at,
    );
    func.blocks[bi].term = Terminator::Branch {
        cond: Operand::Copy(Place::local(need)),
        then: ask,
        els: done,
    };
}

fn int(v: i128, ty: Ty) -> Operand {
    Operand::Const(Const::Int(v), ty)
}

fn new_local(func: &mut Function, ty: Ty, name: &str) -> Local {
    func.locals.push(LocalDecl::new(ty, Some(name.into())));
    Local(func.locals.len() as u32 - 1)
}

/// Append a block (its terminator at `at`); returns its id.
fn add_block(
    func: &mut Function,
    stmts: Vec<Stmt>,
    term: Terminator,
    at: Option<velt_vir::vir::SrcLoc>,
) -> BlockId {
    if !func.locs.is_empty() {
        let mut locs = vec![None; stmts.len()];
        locs.push(at);
        func.locs.push(locs);
    }
    func.blocks.push(BasicBlock { stmts, term });
    BlockId(func.blocks.len() as u32 - 1)
}

#[cfg(test)]
#[path = "module_state_tests.rs"]
mod tests;
