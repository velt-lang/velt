//! The allocator through `declare c` (#2196): each call of the runtime's allocator
//! (`velt_rt_alloc`, `velt_rt_realloc`, `velt_rt_free`; rt_abi.md "Memory") becomes a call of
//! std's hook for it (std/lang/hooks/heap.velt: mimalloc, with the zero-size, length-limit and
//! out-of-memory checks in Velt), which the backend inlines. Lowering puts the hooks in the
//! program only when the build asks (`velt_vir::LowerOptions::heap_hooks`: release builds for
//! native targets), so without them this does nothing.
//!
//! It runs last, once no pass reads the allocator calls any more (`heap_sroa`, `dead_fills`,
//! `vtable_loads`, … find them by the runtime's symbols), and until then [`dead_funcs`]
//! (crate::dead_funcs) keeps a hook alive while a live function calls its extern.
//!
//! The hooks take and give addresses as `u64`, so a pointer argument is cast to `u64` before the
//! call and the result back to `ptr` in a block of its own after it.

use velt_vir::vir::{
    BasicBlock, BlockId, Callee, ExternFn, ExternId, FuncId, Function, LocalDecl, Operand, Place,
    Program, Rvalue, Stmt, Terminator, Ty,
};

/// The runtime allocator's externs and std's hook for each.
const HOOKS: [(&str, &str); 3] = [
    ("velt_rt_alloc", velt_common::std_hook!(heap_alloc)),
    ("velt_rt_realloc", velt_common::std_hook!(heap_realloc)),
    ("velt_rt_free", velt_common::std_hook!(heap_free)),
];

/// The allocator externs `program` declares, each with the hook the program has for it.
pub(crate) fn pairs(program: &Program) -> Vec<(ExternId, FuncId)> {
    if program.externs.is_empty() {
        return vec![];
    }
    let mut out = vec![];
    for (sym, hook) in HOOKS {
        let Some(e) = program.externs.iter().position(|e| e.symbol == sym) else {
            continue;
        };
        let symbol = velt_vir::mangle::mangle(hook);
        let Some(f) = program.funcs.iter().position(|f| f.symbol == symbol) else {
            continue;
        };
        out.push((ExternId(e as u32), FuncId(f as u32)));
    }
    out
}

/// Turns every call of an allocator extern that has its hook into a call of the hook; whether
/// any changed.
pub(crate) fn run(program: &mut Program) -> bool {
    let pairs = pairs(program);
    if pairs.is_empty() {
        return false;
    }
    let mut changed = false;
    for func in &mut program.funcs {
        changed |= run_func(&pairs, &program.externs, func);
    }
    changed
}

fn run_func(pairs: &[(ExternId, FuncId)], externs: &[ExternFn], func: &mut Function) -> bool {
    let mut changed = false;
    for b in 0..func.blocks.len() {
        let Terminator::Call {
            callee: Callee::Extern(e),
            ..
        } = &func.blocks[b].term
        else {
            continue;
        };
        let Some(&(e, hook)) = pairs.iter().find(|(x, _)| x == e) else {
            continue;
        };
        rewrite(func, b, &externs[e.0 as usize].params, hook);
        changed = true;
    }
    changed
}

/// The call ending block `b` made a call of `hook`, its pointer operands (`params` are the
/// extern's) cast to `u64` and its pointer result cast back in a new block before `next`.
fn rewrite(func: &mut Function, b: usize, params: &[Ty], hook: FuncId) {
    let has_locs = !func.locs.is_empty();
    let term_loc = has_locs
        .then(|| func.locs[b].last().cloned().flatten())
        .flatten();
    let Terminator::Call {
        args, dest, next, ..
    } = &func.blocks[b].term
    else {
        unreachable!("ICE: heap_calls rewrites a call")
    };
    let (args, dest, next) = (args.clone(), dest.clone(), *next);
    let mut stmts = vec![];
    let mut new_args = Vec::with_capacity(args.len());
    for (a, ty) in args.into_iter().zip(params) {
        if *ty == Ty::Ptr {
            let t = new_local(func, Ty::U64);
            stmts.push(Stmt::Assign(Place::local(t), Rvalue::Cast(a, Ty::U64)));
            new_args.push(Operand::Copy(Place::local(t)));
        } else {
            new_args.push(a);
        }
    }
    let (new_dest, new_next) = match dest {
        Some(d) => {
            let t = new_local(func, Ty::U64);
            let back = BlockId(func.blocks.len() as u32);
            func.blocks.push(BasicBlock {
                stmts: vec![Stmt::Assign(
                    d,
                    Rvalue::Cast(Operand::Copy(Place::local(t)), Ty::Ptr),
                )],
                term: Terminator::Goto(next),
            });
            if has_locs {
                func.locs.push(vec![term_loc.clone(), term_loc.clone()]);
            }
            (Some(Place::local(t)), back)
        }
        None => (None, next),
    };
    if has_locs {
        let locs = &mut func.locs[b];
        let at = locs.len().saturating_sub(1);
        for _ in &stmts {
            locs.insert(at, term_loc.clone());
        }
    }
    let block = &mut func.blocks[b];
    block.stmts.extend(stmts);
    let Terminator::Call {
        callee,
        args,
        dest,
        next,
        ..
    } = &mut block.term
    else {
        unreachable!("ICE: heap_calls rewrites a call")
    };
    *callee = Callee::Func(hook);
    *args = new_args;
    *dest = new_dest;
    *next = new_next;
}

fn new_local(func: &mut Function, ty: Ty) -> velt_vir::vir::Local {
    func.locals.push(LocalDecl::new(ty, None));
    velt_vir::vir::Local(func.locals.len() as u32 - 1)
}
