//! Splicing one callee body into a call site: callee locals and blocks are appended to the
//! caller (renumbered), parameters become locals assigned from the arguments at the call
//! block, and each `Return(v)` becomes `dest = v; goto next`.

use velt_vir::vir::{
    AggLayout, BlockId, Callee, ExternFn, ExternId, Function, Local, Operand, Place, Rvalue, Stmt,
    Terminator, Ty,
};

use crate::locals::place_ty;
use crate::srclocs::push_stmt;
use crate::visit::{places_mut, successors_mut};

/// The program's `velt_eh_resume` extern, if it declares one (vir.rs invariant 14).
pub(super) fn resume_extern(externs: &[ExternFn]) -> Option<ExternId> {
    (externs.iter())
        .position(|e| velt_vir::eh::is_resume(&e.symbol))
        .map(|i| ExternId(i as u32))
}

/// Inline `callee` at the call terminating block `call_block` of `caller`.
/// The caller must end that block with a direct call to `callee` with matching arity.
/// `resume` is the program's `velt_eh_resume` ([`resume_extern`]): when the call has an unwind
/// edge, the inlined body unwinds to it too (its calls that may unwind and have no edge get the
/// call's, and its resumes jump to the call's pad).
pub(super) fn inline_call(
    aggs: &[AggLayout],
    caller: &mut Function,
    call_block: usize,
    callee: &Function,
    resume: Option<ExternId>,
) {
    let term = std::mem::replace(&mut caller.blocks[call_block].term, Terminator::Unreachable);
    let Terminator::Call {
        args,
        dest,
        next,
        unwind,
        ..
    } = term
    else {
        unreachable!("ICE: inline_call on a block that does not end in a call")
    };
    // A `Unit` destination (or callee) receives nothing.
    let dest =
        dest.filter(|d| callee.ret != Ty::Unit && place_ty(aggs, caller, d) != Some(Ty::Unit));
    let local_base = caller.locals.len() as u32;
    let block_base = caller.blocks.len() as u32;

    let mut body = callee.clone();
    // Inlined statements keep the callee's locations (a callee without any gets none).
    if !caller.locs.is_empty() && body.locs.is_empty() {
        body.locs = body
            .blocks
            .iter()
            .map(|b| vec![None; b.stmts.len() + 1])
            .collect();
    }
    places_mut(&mut body, &mut |p| p.local = Local(p.local.0 + local_base));
    for bi in 0..body.blocks.len() {
        let block = &mut body.blocks[bi];
        successors_mut(&mut block.term, &mut |b| *b = BlockId(b.0 + block_base));
        if let Terminator::Return(value) = &block.term {
            let assign = dest
                .as_ref()
                .map(|d| Stmt::Assign(d.clone(), Rvalue::Use(value.clone())));
            block.term = Terminator::Goto(next);
            if let Some(assign) = assign {
                let at = body.term_loc(bi);
                push_stmt(&mut body, bi, assign, at);
            }
        }
    }

    if let Some(pad) = unwind {
        for block in &mut body.blocks {
            if let Terminator::Call {
                callee: c,
                unwind: u,
                ..
            } = &mut block.term
            {
                if matches!(c, Callee::Extern(id) if Some(*id) == resume) {
                    block.term = Terminator::Goto(pad);
                } else if u.is_none() && velt_vir::eh::may_unwind(c) {
                    *u = Some(pad);
                }
            }
        }
    }

    let call_loc = caller.term_loc(call_block);
    for (i, arg) in args.into_iter().enumerate() {
        let param = Place::local(Local(local_base + i as u32));
        push_stmt(
            caller,
            call_block,
            Stmt::Assign(param, Rvalue::Use(arg)),
            call_loc,
        );
    }
    caller.blocks[call_block].term = Terminator::Goto(BlockId(block_base));
    // Inlined variables are not described: without inlined scopes a debugger would show them
    // as the caller's own.
    caller.locals.extend(body.locals.into_iter().map(|mut l| {
        l.debug = None;
        l
    }));
    caller.blocks.extend(body.blocks);
    if !caller.locs.is_empty() {
        caller.locs.extend(body.locs);
    }
}

/// Whether an operand list matches a parameter list in length (types are the caller's
/// contract; arity is checked defensively since a mismatch would corrupt locals).
pub(super) fn arity_matches(args: &[Operand], callee: &Function) -> bool {
    args.len() == callee.params.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use crate::testkit::validate::assert_valid;
    use velt_vir::vir::{BinOp, Callee, FuncId};

    #[test]
    fn splices_body_with_params_and_returns() {
        let mut pb = ProgramBuilder::new();
        // g(a, b) = if a < b { a } else { b }
        let mut g = FuncBuilder::internal("g", &[Ty::I64, Ty::I64], Ty::I64);
        let (a, b) = (g.param(0), g.param(1));
        let c = g.local(Ty::Bool);
        let (g0, g1, g2) = (g.block(), g.block(), g.block());
        g.assign(g0, c, bin(BinOp::Lt, copy_local(a), copy_local(b)));
        g.branch(g0, c, g1, g2);
        g.ret(g1, copy_local(a));
        g.ret(g2, copy_local(b));
        let gid = pb.add(g.finish());
        // f(x) = g(x, 10) + 1
        let mut f = FuncBuilder::export("f", &[Ty::I64], Ty::I64);
        let x = f.param(0);
        let (r, s) = (f.local(Ty::I64), f.local(Ty::I64));
        let f0 = f.block();
        let f1 = f.call(
            f0,
            Callee::Func(gid),
            vec![copy_local(x), int(10, Ty::I64)],
            Some(r),
        );
        f.assign(f1, s, bin(BinOp::Add, copy_local(r), int(1, Ty::I64)));
        f.ret(f1, copy_local(s));
        pb.add(f.finish());
        let mut p = pb.finish();
        let callee = p.funcs[0].clone();
        inline_call(&p.aggs, &mut p.funcs[1], 0, &callee, None);
        assert_valid(&p);
        let f = &p.funcs[1];
        assert_eq!(f.locals.len(), 3 + 3);
        assert_eq!(f.blocks.len(), 2 + 3);
        assert_eq!(f.blocks[0].term, Terminator::Goto(BlockId(2)));
        assert_eq!(
            f.blocks[3].stmts,
            vec![Stmt::Assign(
                Place::local(r),
                Rvalue::Use(copy_local(Local(3)))
            )]
        );
        assert_eq!(f.blocks[4].term, Terminator::Goto(BlockId(1)));
        assert!(!matches!(
            f.blocks[0].term,
            Terminator::Call {
                callee: Callee::Func(FuncId(0)),
                ..
            }
        ));
    }

    fn call(callee: Callee, next: u32, unwind: Option<u32>) -> Terminator {
        Terminator::Call {
            callee,
            args: vec![],
            dest: None,
            next: BlockId(next),
            unwind: unwind.map(BlockId),
        }
    }

    fn function(name: &str, terms: Vec<Terminator>) -> Function {
        let blocks = (terms.into_iter())
            .map(|term| velt_vir::vir::BasicBlock {
                stmts: vec![],
                term,
            })
            .collect();
        Function {
            blocks,
            ..Function::new(
                name.into(),
                vec![],
                Ty::Unit,
                velt_vir::vir::Linkage::Internal,
            )
        }
    }

    /// A call with an unwind edge (vir.rs invariant 14) inlines into it: the body's calls without
    /// an edge get the call's pad, its own pads stay, and its resumes jump to the call's pad.
    #[test]
    fn an_inlined_body_unwinds_to_the_calls_pad() {
        let (k, resume) = (Callee::Func(FuncId(2)), ExternId(0));
        let r = || Callee::Extern(resume);
        // g: k(); k() unwind bb3; return; bb3: resume.
        let g = function(
            "g",
            vec![
                call(k.clone(), 1, None),
                call(k.clone(), 2, Some(3)),
                Terminator::Return(Operand::Const(velt_vir::vir::Const::Unit, Ty::Unit)),
                call(r(), 4, None),
                Terminator::Unreachable,
            ],
        );
        // f: g() unwind bb2; return; bb2: resume.
        let mut f = function(
            "f",
            vec![
                call(Callee::Func(FuncId(0)), 1, Some(2)),
                Terminator::Return(Operand::Const(velt_vir::vir::Const::Unit, Ty::Unit)),
                call(r(), 3, None),
                Terminator::Unreachable,
            ],
        );
        inline_call(&[], &mut f, 0, &g, Some(resume));
        assert_eq!(f.blocks[0].term, Terminator::Goto(BlockId(4)));
        assert_eq!(f.blocks[4].term, call(k.clone(), 5, Some(2)));
        assert_eq!(f.blocks[5].term, call(k.clone(), 6, Some(7)));
        assert_eq!(f.blocks[7].term, Terminator::Goto(BlockId(2)));
        // Without an edge the body stays as it is.
        let mut f = function(
            "f",
            vec![
                call(Callee::Func(FuncId(0)), 1, None),
                Terminator::Return(Operand::Const(velt_vir::vir::Const::Unit, Ty::Unit)),
            ],
        );
        inline_call(&[], &mut f, 0, &g, Some(resume));
        assert_eq!(f.blocks[2].term, call(k.clone(), 3, None));
        assert_eq!(f.blocks[5].term, call(r(), 6, None));
    }
}
