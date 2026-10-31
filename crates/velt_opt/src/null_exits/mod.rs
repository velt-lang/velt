//! A call of a function that returns at once when a pointer it is passed points at null tests
//! that pointer itself first (`null_exits`).
//!
//! Every drop glue starts by reading the reference it releases and returning when it is null
//! (`_Gdrop_…(slot)`: `p = *slot; if p == null return`): a field of a `T | null` type, a value
//! moved out, an object's fields in its zero fill. A glue that releases a class with many fields
//! is too large for LLVM to inline, so dropping a null `Tape | null` (velt:json's `JsonValue` off
//! a tape) cost a call and the glue's saving of six registers before its first test, at every
//! drop of every `JsonValue`.
//!
//! The call `f(…, slot, …)` becomes `if (*slot != null) f$present(…, slot, …)`, where
//! `f$present` is `f` without its test (its entry block goes straight on to the work). Where
//! the value is known (a null just stored, the other edge of a test of it) the backends then drop
//! the call; a null elsewhere costs a load and a branch instead of a call; and a value that is
//! not null is tested once, in the caller, as it was in the callee before. The callee qualifies
//! when its entry block only reads `*param`, compares it with null and branches to a block that
//! returns `()` with nothing else, besides constants assigned to locals (which the early return
//! does not use): skipping the call where the pointer is null does exactly what the call would
//! have done, and the clone does what the call does where it is not.

#[cfg(test)]
mod tests;

use velt_vir::vir::{
    BasicBlock, BinOp, BlockId, Callee, Const, FuncId, Function, Linkage, LocalDecl, Operand,
    Place, Program, Proj, Rvalue, Stmt, Terminator, Ty,
};

use crate::srclocs::push_stmt;

/// Test the pointee of each call's argument that makes its callee return at once, and call the
/// callee's clone without the test where it is not null; returns whether any call changed.
pub(crate) fn run(program: &mut Program) -> bool {
    let exits: Vec<Option<(usize, BlockId)>> = program.funcs.iter().map(null_exit).collect();
    let mut present: Vec<Option<FuncId>> = vec![None; exits.len()];
    let mut changed = false;
    // The clones are visited too: their calls of other glue are tested the same way.
    let mut i = 0;
    while i < program.funcs.len() {
        let sites = sites(&exits, &program.funcs[i]);
        for &(bi, f, k) in &sites {
            let body = match present[f] {
                Some(id) => id,
                None => {
                    let id = clone_present(program, f, exits[f].map(|(_, b)| b));
                    present[f] = Some(id);
                    id
                }
            };
            guard(&mut program.funcs[i], bi, k, body);
            changed = true;
        }
        i += 1;
    }
    changed
}

/// The calls of `func` that `run` guards: (block, callee, parameter).
fn sites(exits: &[Option<(usize, BlockId)>], func: &Function) -> Vec<(usize, usize, usize)> {
    let mut out = vec![];
    for (bi, b) in func.blocks.iter().enumerate() {
        let Terminator::Call {
            callee: Callee::Func(f),
            args,
            dest: None,
            ..
        } = &b.term
        else {
            continue;
        };
        let Some((k, _)) = exits.get(f.0 as usize).copied().flatten() else {
            continue;
        };
        // A plain pointer local (a null slot pointer is never passed: the callee reads it).
        let fits = matches!(args.get(k), Some(Operand::Copy(p))
            if p.proj.is_empty() && func.locals[p.local.0 as usize].ty == Ty::Ptr);
        if fits {
            out.push((bi, f.0 as usize, k));
        }
    }
    out
}

/// `f` without its test: its entry block goes on to `body` (where the pointee is not null).
fn clone_present(program: &mut Program, f: usize, body: Option<BlockId>) -> FuncId {
    let body = body.unwrap_or_else(|| panic!("ICE: null_exits: no body block"));
    let mut g = program.funcs[f].clone();
    g.symbol = format!("{}$present", g.symbol);
    g.linkage = Linkage::Internal;
    g.blocks[0].term = Terminator::Goto(body);
    // The early return is now unreachable.
    crate::simplify_cfg::run(&mut g);
    let id = FuncId(program.funcs.len() as u32);
    program.funcs.push(g);
    id
}

/// The parameter `k` such that `f` returns `()` at once, doing nothing, when `*param k` is null,
/// and the block it goes on to when it is not.
fn null_exit(f: &Function) -> Option<(usize, BlockId)> {
    if f.ret != Ty::Unit || f.cold || f.is_poll || f.blocks.is_empty() {
        return None;
    }
    let entry = &f.blocks[0];
    let Terminator::Branch {
        cond: Operand::Copy(c),
        then,
        els,
    } = &entry.term
    else {
        return None;
    };
    if !c.proj.is_empty() {
        return None;
    }
    // The test: `t = *param`, then `c = ne t, 0` (or `eq`), and the rest constants into other
    // locals (not a parameter, `t` or `c`, which would change what is read or tested).
    let mut read = None;
    let mut test = None;
    let mut consts = vec![];
    for s in &entry.stmts {
        let Stmt::Assign(pl, rv) = s else {
            return None;
        };
        if !pl.proj.is_empty() {
            return None;
        }
        match rv {
            Rvalue::Use(Operand::Const(..)) => consts.push(pl.local),
            Rvalue::Use(Operand::Copy(src))
                if read.is_none()
                    && src.proj == [Proj::Deref(Ty::Ptr)]
                    && (src.local.0 as usize) < f.params.len()
                    && f.params[src.local.0 as usize] == Ty::Ptr =>
            {
                read = Some((pl.local, src.local.0 as usize));
            }
            // The comparison comes after the read: it tests the value read.
            Rvalue::Binary(op @ (BinOp::Ne | BinOp::Eq), a, b)
                if test.is_none() && read.is_some() =>
            {
                let null = |o: &Operand| matches!(o, Operand::Const(Const::Int(0), Ty::Ptr));
                let t = match (a, b) {
                    (Operand::Copy(x), y) | (y, Operand::Copy(x))
                        if null(y) && x.proj.is_empty() =>
                    {
                        x.local
                    }
                    _ => return None,
                };
                test = Some((pl.local, t, *op));
            }
            _ => return None,
        }
    }
    let ((t, k), (cl, tested, op)) = (read?, test?);
    if tested != t || cl != c.local {
        return None;
    }
    let overwrites = |l: velt_vir::vir::Local| (l.0 as usize) < f.params.len() || l == t || l == cl;
    if consts.into_iter().any(overwrites) {
        return None;
    }
    // Every other statement assigns a constant; none may overwrite the test's locals after it.
    let assigned = |l| {
        (entry.stmts.iter())
            .filter(|s| matches!(s, Stmt::Assign(p, _) if p.local == l))
            .count()
    };
    if assigned(t) != 1 || assigned(cl) != 1 {
        return None;
    }
    let (exit, body) = match op {
        BinOp::Ne => (*els, *then),
        _ => (*then, *els),
    };
    let b = f.blocks.get(exit.0 as usize)?;
    let returns =
        b.stmts.is_empty() && matches!(&b.term, Terminator::Return(Operand::Const(Const::Unit, _)));
    (returns && exit != body).then_some((k, body))
}

/// Make the call ending block `bi` of `func` `if (*slot != null) body(…)`, `slot` its argument
/// `k`.
fn guard(func: &mut Function, bi: usize, k: usize, body: FuncId) {
    let Terminator::Call { args, next, .. } = &func.blocks[bi].term else {
        panic!("ICE: null_exits: a site is not a call");
    };
    let next = *next;
    let Some(Operand::Copy(slot)) = args.get(k).cloned() else {
        panic!("ICE: null_exits: the slot is not a local");
    };
    let at = func.term_loc(bi);
    func.locals.push(LocalDecl::new(Ty::Ptr, None));
    let t = Place::local(velt_vir::vir::Local(func.locals.len() as u32 - 1));
    func.locals.push(LocalDecl::new(Ty::Bool, None));
    let c = Place::local(velt_vir::vir::Local(func.locals.len() as u32 - 1));
    let call = BlockId(func.blocks.len() as u32);
    let mut term = std::mem::replace(
        &mut func.blocks[bi].term,
        Terminator::Branch {
            cond: Operand::Copy(c.clone()),
            then: call,
            els: next,
        },
    );
    if let Terminator::Call { callee, .. } = &mut term {
        *callee = Callee::Func(body);
    }
    func.blocks.push(BasicBlock {
        stmts: Vec::new(),
        term,
    });
    if !func.locs.is_empty() {
        func.locs.push(vec![at]);
    }
    let mut pointee = slot;
    pointee.proj.push(Proj::Deref(Ty::Ptr));
    let read = Stmt::Assign(t.clone(), Rvalue::Use(Operand::Copy(pointee)));
    push_stmt(func, bi, read, at);
    let null = Operand::Const(Const::Int(0), Ty::Ptr);
    let test = Stmt::Assign(c, Rvalue::Binary(BinOp::Ne, Operand::Copy(t), null));
    push_stmt(func, bi, test, at);
}
