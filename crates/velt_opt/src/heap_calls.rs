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
const HOOKS: [(&str, &str); 2] = [
    ("velt_rt_alloc", velt_common::std_hook!(heap_alloc)),
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
    let hooks: Vec<FuncId> = pairs.iter().map(|&(_, h)| h).collect();
    for (i, func) in program.funcs.iter_mut().enumerate() {
        // A hook's own calls stay (it is what they become).
        if !hooks.contains(&FuncId(i as u32)) {
            changed |= run_func(&pairs, &program.externs, func);
        }
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
                func.locs.push(vec![term_loc, term_loc]);
            }
            (Some(Place::local(t)), back)
        }
        None => (None, next),
    };
    if has_locs {
        let locs = &mut func.locs[b];
        let at = locs.len().saturating_sub(1);
        for _ in &stmts {
            locs.insert(at, term_loc);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use crate::testkit::validate::assert_valid;

    fn hook(name: &str, params: &[Ty], ret: Ty) -> Function {
        let mut fb = FuncBuilder::internal(&velt_vir::mangle::mangle(name), params, ret);
        let b = fb.block();
        let out = match ret {
            Ty::Unit => Operand::Const(velt_vir::vir::Const::Unit, Ty::Unit),
            _ => copy_local(fb.param(0)),
        };
        fb.ret(b, out);
        fb.finish()
    }

    /// `main` allocates, grows and frees through the runtime's externs; with `hooks` the program
    /// has std's hooks too.
    fn program(hooks: bool) -> Program {
        let mut pb = ProgramBuilder::new();
        let alloc = pb.ext("velt_rt_alloc", &[Ty::U64, Ty::U64], Ty::Ptr, false);
        let realloc = pb.ext(
            "velt_rt_realloc",
            &[Ty::Ptr, Ty::U64, Ty::U64, Ty::U64],
            Ty::Ptr,
            false,
        );
        let free = pb.ext(
            "velt_rt_free",
            &[Ty::Ptr, Ty::U64, Ty::U64],
            Ty::Unit,
            false,
        );
        if hooks {
            let u = Ty::U64;
            pb.add(hook(velt_common::std_hook!(heap_alloc), &[u, u], u));
            pb.add(hook(
                velt_common::std_hook!(heap_free),
                &[u, u, u],
                Ty::Unit,
            ));
        }
        let mut fb = FuncBuilder::export("main", &[], Ty::Unit);
        let p = fb.local(Ty::Ptr);
        let b = fb.block();
        let args = vec![int(16, Ty::U64), int(8, Ty::U64)];
        let b = fb.call(b, Callee::Extern(alloc), args, Some(p));
        let args = vec![
            copy_local(p),
            int(16, Ty::U64),
            int(8, Ty::U64),
            int(64, Ty::U64),
        ];
        let b = fb.call(b, Callee::Extern(realloc), args, Some(p));
        let args = vec![copy_local(p), int(64, Ty::U64), int(8, Ty::U64)];
        let b = fb.call(b, Callee::Extern(free), args, None);
        fb.ret(b, Operand::Const(velt_vir::vir::Const::Unit, Ty::Unit));
        pb.add(fb.finish());
        pb.finish()
    }

    fn callees(f: &Function) -> Vec<Callee> {
        f.blocks
            .iter()
            .filter_map(|b| match &b.term {
                Terminator::Call { callee, .. } => Some(callee.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn each_allocator_call_becomes_its_hooks() {
        let mut p = program(true);
        assert!(run(&mut p));
        assert_valid(&p);
        let main = p.funcs.iter().find(|f| f.symbol == "main").unwrap();
        assert_eq!(
            callees(main),
            [
                Callee::Func(FuncId(0)),
                Callee::Extern(ExternId(1)),
                Callee::Func(FuncId(1))
            ]
        );
        // The reallocation stays the runtime's. The pointer argument of the free and the
        // allocation's result, cast.
        let casts = main.blocks.iter().flat_map(|b| &b.stmts);
        assert_eq!(
            casts
                .filter(|s| matches!(s, Stmt::Assign(_, Rvalue::Cast(..))))
                .count(),
            2
        );
        // Nothing left to do.
        assert!(!run(&mut p));
    }

    #[test]
    fn without_the_hooks_nothing_changes() {
        let mut p = program(false);
        assert!(!run(&mut p));
        let main = p.funcs.iter().find(|f| f.symbol == "main").unwrap();
        assert!(callees(main).iter().all(|c| matches!(c, Callee::Extern(_))));
    }

    #[test]
    fn unused_functions_keep_a_hook_while_its_extern_is_called() {
        let mut p = program(true);
        crate::dead_funcs::run(&mut p);
        assert_eq!(p.funcs.len(), 3, "the hooks stay for heap_calls");
        run(&mut p);
        crate::dead_funcs::run(&mut p);
        assert_eq!(p.funcs.len(), 3, "the hooks are called");
        assert_valid(&p);
    }
}
