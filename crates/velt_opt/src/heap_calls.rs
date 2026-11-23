//! The allocator through `declare c` (#2196): calls of the runtime's allocator (`velt_rt_alloc`,
//! `velt_rt_free`; rt_abi.md "Memory") become calls of std's hooks for them
//! (std/lang/hooks/heap.velt: mimalloc, with the zero-size and out-of-memory checks in Velt),
//! which the backend inlines. Lowering puts the hooks in the program only when the build asks
//! (`velt_vir::LowerOptions::heap_hooks`: release builds for native targets), so without them
//! this does nothing.
//!
//! An allocation of a size that is a constant here (objects, closure environments, counted
//! boxes) calls the hook itself, which folds to the mimalloc call and its null check. An array
//! buffer of a run-time length and array growth (`velt_rt_realloc`) call a wrapper of the hook
//! instead ([`ALLOC_WRAPPER`], [`REALLOC_WRAPPER`]): a function with the runtime function's own
//! signature that calls the hook (inlined into it) and stays out of line. velt_codegen_llvm gives
//! it the runtime function's allocator attributes (rt_abi.md "Memory"), so the loops that fill
//! or push into such a buffer see the same opaque `allockind` call as before. Both halves
//! matter (#3316, by instructions):
//! - **Inlined:** with the hook inlined at each site, LLVM compiled those loops differently (a
//!   peeled first iteration and two more induction variables in the hot path; bench/hashmap.velt
//!   +4.7% and bench/closures.velt +4.1%). With the wrapper inlinable, bench/json/parse_value.velt
//!   rose 4.2% on arm64.
//! - **Without the attributes:** an out-of-line wrapper without them cost
//!   bench/permutations.velt 3.8% on arm64 (nothing on x86-64).
//!
//! It runs last, once no pass reads the allocator calls any more (`heap_sroa`, `dead_fills`,
//! `vtable_loads`, … find them by the runtime's symbols), and until then [`dead_funcs`]
//! (crate::dead_funcs) keeps a hook alive while a live function calls its extern.
//!
//! The hooks take and give addresses as `u64`, so a pointer argument is cast to `u64` before the
//! call and the result back to `ptr` in a block of its own after it.

use velt_vir::vir::{
    BasicBlock, BlockId, Callee, ExternFn, ExternId, FuncId, Function, Inline, Linkage, LocalDecl,
    Operand, Place, Program, Rvalue, Stmt, Terminator, Ty,
};

/// The runtime allocator's externs and std's hook for each.
const HOOKS: [(&str, &str); 3] = [
    ("velt_rt_alloc", velt_common::std_hook!(heap_alloc)),
    ("velt_rt_realloc", velt_common::std_hook!(heap_realloc)),
    ("velt_rt_free", velt_common::std_hook!(heap_free)),
];

use velt_vir::heap::{ALLOC_WRAPPER, REALLOC_WRAPPER};

/// The allocator externs `program` declares, each with the hook the program has for it: none
/// where lowering put no hooks in the program (`velt_vir::LowerOptions::heap_hooks`).
pub fn pairs(program: &Program) -> Vec<(ExternId, FuncId)> {
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

/// Std's heap hooks that take the place of the allocator externs `f` calls at the end ([`run`]),
/// given the program's [`pairs`]: what `f`'s code reaches beyond its own calls (`dead_funcs`).
pub(crate) fn hooks_of(f: &Function, pairs: &[(ExternId, FuncId)]) -> Vec<FuncId> {
    (pairs.iter())
        .filter(|&&(e, _)| calls_extern(f, e))
        .map(|&(_, h)| h)
        .collect()
}

/// Whether `f` calls extern `e`.
pub(crate) fn calls_extern(f: &Function, e: ExternId) -> bool {
    (f.blocks.iter())
        .any(|b| matches!(b.term, Terminator::Call { callee: Callee::Extern(x), .. } if x == e))
}

/// Turns every call of an allocator extern that has its hook into a call of the hook; whether
/// any changed.
pub(crate) fn run(program: &mut Program) -> bool {
    let pairs = pairs(program);
    if pairs.is_empty() {
        return false;
    }
    let hooks: Vec<FuncId> = pairs.iter().map(|&(_, h)| h).collect();
    // A hook's own calls stay (it is what they become).
    let callers = |p: &Program| {
        (0..p.funcs.len() as u32)
            .map(FuncId)
            .filter(|f| !hooks.contains(f))
            .collect::<Vec<_>>()
    };
    // The wrappers the calls need, made first (they call the hooks, not the externs).
    let mut wrappers = Wrappers::default();
    for f in callers(program) {
        for b in &program.funcs[f.0 as usize].blocks {
            if let Some((e, hook)) = paired(&pairs, &b.term) {
                match wrapped(&program.externs[e.0 as usize], &b.term) {
                    Some(ALLOC_WRAPPER) if wrappers.alloc.is_none() => {
                        wrappers.alloc = Some((e, hook));
                    }
                    Some(REALLOC_WRAPPER) if wrappers.realloc.is_none() => {
                        wrappers.realloc = Some((e, hook));
                    }
                    _ => {}
                }
            }
        }
    }
    let alloc = wrappers
        .alloc
        .map(|(e, h)| wrapper_of(program, e, h, ALLOC_WRAPPER));
    let realloc = (wrappers.realloc).map(|(e, h)| wrapper_of(program, e, h, REALLOC_WRAPPER));
    let mut changed = false;
    for f in callers(program) {
        let func = &mut program.funcs[f.0 as usize];
        for b in 0..func.blocks.len() {
            let Some((e, hook)) = paired(&pairs, &func.blocks[b].term) else {
                continue;
            };
            let ext = &program.externs[e.0 as usize];
            let wrapper = match wrapped(ext, &func.blocks[b].term) {
                Some(ALLOC_WRAPPER) => alloc,
                Some(_) => realloc,
                None => None,
            };
            match wrapper {
                Some(w) => {
                    let Terminator::Call { callee, .. } = &mut func.blocks[b].term else {
                        unreachable!("ICE: heap_calls rewrites a call")
                    };
                    *callee = Callee::Func(w);
                }
                None => rewrite(func, b, &ext.params, hook),
            }
            changed = true;
        }
    }
    changed
}

/// The first call of each wrapper (module docs): its extern and that extern's hook.
#[derive(Default)]
struct Wrappers {
    alloc: Option<(ExternId, FuncId)>,
    realloc: Option<(ExternId, FuncId)>,
}

/// The allocator extern `term` calls and its hook, if it calls one that has a hook.
fn paired(pairs: &[(ExternId, FuncId)], term: &Terminator) -> Option<(ExternId, FuncId)> {
    let Terminator::Call {
        callee: Callee::Extern(e),
        ..
    } = term
    else {
        return None;
    };
    pairs.iter().find(|(x, _)| x == e).copied()
}

/// The wrapper the call `term` of `ext` goes through, if not the hook itself (module docs).
fn wrapped(ext: &ExternFn, term: &Terminator) -> Option<&'static str> {
    match ext.symbol.as_str() {
        "velt_rt_alloc" if !constant_size(term) => Some(ALLOC_WRAPPER),
        "velt_rt_realloc" => Some(REALLOC_WRAPPER),
        _ => None,
    }
}

/// Adds the wrapper `symbol` of extern `e`'s `hook` to `program`: `e`'s signature, a body that
/// calls `hook`, out of line (`Inline::Never`).
fn wrapper_of(program: &mut Program, e: ExternId, hook: FuncId, symbol: &str) -> FuncId {
    if let Some(f) = program.funcs.iter().position(|f| f.symbol == symbol) {
        return FuncId(f as u32);
    }
    let ext = &program.externs[e.0 as usize];
    let params = ext.params.clone();
    let mut f = Function::new(
        symbol.to_string(),
        params.clone(),
        ext.ret,
        Linkage::Internal,
    );
    f.inline = Inline::Never;
    let result = new_local(&mut f, ext.ret);
    let args = (0..params.len())
        .map(|i| Operand::Copy(Place::local(velt_vir::vir::Local(i as u32))))
        .collect();
    f.blocks = vec![
        BasicBlock {
            stmts: vec![],
            term: Terminator::Call {
                callee: Callee::Extern(e),
                args,
                dest: Some(Place::local(result)),
                next: BlockId(1),
                unwind: None,
            },
        },
        BasicBlock {
            stmts: vec![],
            term: Terminator::Return(Operand::Copy(Place::local(result))),
        },
    ];
    rewrite(&mut f, 0, &params, hook);
    program.funcs.push(f);
    FuncId(program.funcs.len() as u32 - 1)
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

/// Whether the allocation ending a block asks for a size known here (module docs).
fn constant_size(term: &Terminator) -> bool {
    matches!(
        term,
        Terminator::Call { args, .. } if matches!(args.first(), Some(Operand::Const(..)))
    )
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

    /// `main(n)` allocates 16 bytes, grows them, frees them and allocates `n` bytes through the
    /// runtime's externs; with `hooks` the program has std's hooks too (functions 0 to 2).
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
            let realloc = velt_common::std_hook!(heap_realloc);
            pb.add(hook(realloc, &[u, u, u, u], u));
        }
        let mut fb = FuncBuilder::export("main", &[Ty::U64], Ty::Unit);
        let n = fb.param(0);
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
        let args = vec![copy_local(n), int(8, Ty::U64)];
        let b = fb.call(b, Callee::Extern(alloc), args, Some(p));
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

    fn func<'p>(p: &'p Program, symbol: &str) -> (FuncId, &'p Function) {
        let i = p.funcs.iter().position(|f| f.symbol == symbol).unwrap();
        (FuncId(i as u32), &p.funcs[i])
    }

    #[test]
    fn each_allocator_call_becomes_its_hooks() {
        let mut p = program(true);
        assert!(run(&mut p));
        assert_valid(&p);
        let (alloc, alloc_wrapper) = func(&p, ALLOC_WRAPPER);
        let (realloc, realloc_wrapper) = func(&p, REALLOC_WRAPPER);
        let (_, main) = func(&p, "main");
        // The constant size and the free call their hooks; the growth and the run-time size the
        // wrappers.
        assert_eq!(
            callees(main),
            [
                Callee::Func(FuncId(0)),
                Callee::Func(realloc),
                Callee::Func(FuncId(1)),
                Callee::Func(alloc),
            ]
        );
        // The pointer argument of the free and the first allocation's result, cast.
        let casts = |f: &Function| {
            (f.blocks.iter().flat_map(|b| &b.stmts))
                .filter(|s| matches!(s, Stmt::Assign(_, Rvalue::Cast(..))))
                .count()
        };
        assert_eq!(casts(main), 2);
        // Each wrapper has its runtime function's signature, stays out of line, and calls the
        // hook with its pointers cast: the result, and the block to grow.
        for (w, ext, hook, n) in [(alloc_wrapper, 0, 0, 1), (realloc_wrapper, 1, 2, 2)] {
            let e = &p.externs[ext];
            assert_eq!((&w.params, w.ret), (&e.params, e.ret), "{}", w.symbol);
            assert_eq!(w.inline, Inline::Never);
            assert_eq!(w.linkage, Linkage::Internal);
            assert_eq!(callees(w), [Callee::Func(FuncId(hook))], "{}", w.symbol);
            assert_eq!(casts(w), n, "{}", w.symbol);
        }
        // Nothing left to do.
        assert!(!run(&mut p));
        assert_eq!(p.funcs.len(), 6);
    }

    #[test]
    fn without_the_hooks_nothing_changes() {
        let mut p = program(false);
        assert!(!run(&mut p));
        let main = p.funcs.iter().find(|f| f.symbol == "main").unwrap();
        assert!(callees(main).iter().all(|c| matches!(c, Callee::Extern(_))));
        assert_eq!(p.funcs.len(), 1, "no wrappers");
    }

    #[test]
    fn unused_functions_keep_a_hook_while_its_extern_is_called() {
        let mut p = program(true);
        crate::dead_funcs::run(&mut p);
        assert_eq!(p.funcs.len(), 4, "the hooks stay for heap_calls");
        run(&mut p);
        crate::dead_funcs::run(&mut p);
        assert_eq!(p.funcs.len(), 6, "the hooks and the wrappers are called");
        assert_valid(&p);
    }
}
