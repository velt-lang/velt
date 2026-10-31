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
//! The call `f(…, slot, …)` becomes `if (*slot != null) f(…, slot, …)`. Where the value is known
//! (a null just stored, the other edge of a test of it) the backends then drop the call, and
//! elsewhere a null costs a load and a branch. The callee qualifies when its entry block only
//! reads `*param`, compares it with null and branches to a block that returns `()` with nothing
//! else, besides constants assigned to locals (which the early return does not use): skipping
//! the call where the pointer is null does exactly what the call would have done.

#[cfg(test)]
mod tests;

use velt_vir::vir::{
    BasicBlock, BinOp, BlockId, Callee, Const, Function, LocalDecl, Operand, Place, Program, Proj,
    Rvalue, Stmt, Terminator, Ty,
};

use crate::srclocs::push_stmt;

/// Per function: the parameter whose pointee being null makes it return at once.
pub(crate) struct NullExits(Vec<Option<usize>>);

impl NullExits {
    pub fn find(program: &Program) -> NullExits {
        NullExits(program.funcs.iter().map(null_exit).collect())
    }
}

/// The parameter `k` such that `f` returns `()` at once, doing nothing, when `*param k` is null.
fn null_exit(f: &Function) -> Option<usize> {
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
    let exit = match op {
        BinOp::Ne => *els,
        _ => *then,
    };
    let b = f.blocks.get(exit.0 as usize)?;
    let returns =
        b.stmts.is_empty() && matches!(&b.term, Terminator::Return(Operand::Const(Const::Unit, _)));
    returns.then_some(k)
}

/// Test the pointee of each call's argument that makes its callee return at once; returns
/// whether any call changed.
pub(crate) fn run(exits: &NullExits, func: &mut Function) -> bool {
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        let Terminator::Call {
            callee: Callee::Func(f),
            args,
            dest: None,
            next,
        } = &func.blocks[bi].term
        else {
            continue;
        };
        let Some(k) = exits.0.get(f.0 as usize).copied().flatten() else {
            continue;
        };
        let next = *next;
        let slot = match args.get(k) {
            Some(Operand::Copy(p))
                if p.proj.is_empty() && func.locals[p.local.0 as usize].ty == Ty::Ptr =>
            {
                p.clone()
            }
            // A null slot pointer is never passed (the callee reads through it).
            _ => continue,
        };
        let at = func.term_loc(bi);
        func.locals.push(LocalDecl::new(Ty::Ptr, None));
        let t = Place::local(velt_vir::vir::Local(func.locals.len() as u32 - 1));
        func.locals.push(LocalDecl::new(Ty::Bool, None));
        let c = Place::local(velt_vir::vir::Local(func.locals.len() as u32 - 1));
        let call = BlockId(func.blocks.len() as u32);
        let term = std::mem::replace(
            &mut func.blocks[bi].term,
            Terminator::Branch {
                cond: Operand::Copy(c.clone()),
                then: call,
                els: next,
            },
        );
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
        changed = true;
    }
    changed
}
