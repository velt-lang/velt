//! Structural traversal of VIR shared by all passes: operands read by statements and
//! terminators, places written, and CFG successors. Keeping this in one place means a new
//! VIR construct only has to be taught to the optimizer here.

use velt_vir::vir::{BlockId, Callee, Function, Operand, Place, Proj, Rvalue, Stmt, Terminator};

/// Operands read by an rvalue (not the `AddrOf` place: taking an address reads nothing).
pub(crate) fn rvalue_operands(rv: &Rvalue, f: &mut impl FnMut(&Operand)) {
    match rv {
        Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => f(a),
        Rvalue::Binary(_, a, b) => {
            f(a);
            f(b);
        }
        Rvalue::Aggregate(_, ops) => ops.iter().for_each(f),
        Rvalue::AddrOf(_) => {}
    }
}

/// Mutable variant of `rvalue_operands`.
pub(crate) fn rvalue_operands_mut(rv: &mut Rvalue, f: &mut impl FnMut(&mut Operand)) {
    match rv {
        Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => f(a),
        Rvalue::Binary(_, a, b) => {
            f(a);
            f(b);
        }
        Rvalue::Aggregate(_, ops) => ops.iter_mut().for_each(f),
        Rvalue::AddrOf(_) => {}
    }
}

/// Operands read by a statement.
pub(crate) fn stmt_operands(s: &Stmt, f: &mut impl FnMut(&Operand)) {
    match s {
        Stmt::Assign(_, rv) => rvalue_operands(rv, f),
        Stmt::MemCopy { dst, src, .. } => {
            f(dst);
            f(src);
        }
        Stmt::MemCopyDyn { dst, src, len, .. } => {
            f(dst);
            f(src);
            f(len);
        }
        Stmt::MemSet { dst, byte, len } => {
            f(dst);
            f(byte);
            f(len);
        }
        Stmt::Nop => {}
    }
}

/// Mutable variant of `stmt_operands`.
pub(crate) fn stmt_operands_mut(s: &mut Stmt, f: &mut impl FnMut(&mut Operand)) {
    match s {
        Stmt::Assign(_, rv) => rvalue_operands_mut(rv, f),
        Stmt::MemCopy { dst, src, .. } => {
            f(dst);
            f(src);
        }
        Stmt::MemCopyDyn { dst, src, len, .. } => {
            f(dst);
            f(src);
            f(len);
        }
        Stmt::MemSet { dst, byte, len } => {
            f(dst);
            f(byte);
            f(len);
        }
        Stmt::Nop => {}
    }
}

/// Operands read by a terminator (branch conditions, call targets and arguments, returns).
pub(crate) fn term_operands(t: &Terminator, f: &mut impl FnMut(&Operand)) {
    match t {
        Terminator::Branch { cond, .. } => f(cond),
        Terminator::Switch { value, .. } => f(value),
        Terminator::Return(op) => f(op),
        Terminator::Call { callee, args, .. } => {
            if let Callee::Ptr { target, .. } = callee {
                f(target);
            }
            args.iter().for_each(f);
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}

/// Mutable variant of `term_operands`.
pub(crate) fn term_operands_mut(t: &mut Terminator, f: &mut impl FnMut(&mut Operand)) {
    match t {
        Terminator::Branch { cond, .. } => f(cond),
        Terminator::Switch { value, .. } => f(value),
        Terminator::Return(op) => f(op),
        Terminator::Call { callee, args, .. } => {
            if let Callee::Ptr { target, .. } = callee {
                f(target);
            }
            args.iter_mut().for_each(f);
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}

/// How a place occurrence uses its base local.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaceUse {
    /// Read through an operand.
    Read,
    /// Destination of an assignment or call.
    Write,
    /// Operand of `AddrOf`.
    AddrOf,
}

/// Every place mentioned by a statement, with how it is used.
pub(crate) fn stmt_places(s: &Stmt, f: &mut impl FnMut(&Place, PlaceUse)) {
    stmt_operands(s, &mut |op| {
        if let Operand::Copy(p) = op {
            f(p, PlaceUse::Read);
        }
    });
    if let Stmt::Assign(place, rv) = s {
        if let Rvalue::AddrOf(p) = rv {
            f(p, PlaceUse::AddrOf);
        }
        f(place, PlaceUse::Write);
    }
}

/// Every place mentioned by a terminator, with how it is used.
pub(crate) fn term_places(t: &Terminator, f: &mut impl FnMut(&Place, PlaceUse)) {
    term_operands(t, &mut |op| {
        if let Operand::Copy(p) = op {
            f(p, PlaceUse::Read);
        }
    });
    if let Terminator::Call { dest: Some(d), .. } = t {
        f(d, PlaceUse::Write);
    }
}

/// Whether the place goes through a pointer: then its base local is only *read* (for the
/// pointer value), whatever the place is used for.
pub(crate) fn derefs(p: &Place) -> bool {
    p.proj.iter().any(|x| matches!(x, Proj::Deref(_)))
}

/// Successor blocks of a terminator (may contain duplicates).
pub(crate) fn successors(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Goto(b) => vec![*b],
        Terminator::Branch { then, els, .. } => vec![*then, *els],
        Terminator::Switch { cases, default, .. } => {
            let mut out: Vec<BlockId> = cases.iter().map(|(_, b)| *b).collect();
            out.push(*default);
            out
        }
        Terminator::Call { next, .. } => vec![*next],
        Terminator::Return(_) | Terminator::Unreachable => vec![],
    }
}

/// Visit every successor edge of a terminator mutably.
pub(crate) fn successors_mut(t: &mut Terminator, f: &mut impl FnMut(&mut BlockId)) {
    match t {
        Terminator::Goto(b) => f(b),
        Terminator::Branch { then, els, .. } => {
            f(then);
            f(els);
        }
        Terminator::Switch { cases, default, .. } => {
            cases.iter_mut().for_each(|(_, b)| f(b));
            f(default);
        }
        Terminator::Call { next, .. } => f(next),
        Terminator::Return(_) | Terminator::Unreachable => {}
    }
}

/// Every place in the function mutably (operands, destinations, `AddrOf` places), so passes
/// can rename locals wholesale.
pub(crate) fn places_mut(func: &mut Function, f: &mut impl FnMut(&mut Place)) {
    for block in &mut func.blocks {
        for s in &mut block.stmts {
            stmt_operands_mut(s, &mut |op| operand_place(op, f));
            if let Stmt::Assign(place, rv) = s {
                if let Rvalue::AddrOf(p) = rv {
                    f(p);
                }
                f(place);
            }
        }
        term_operands_mut(&mut block.term, &mut |op| operand_place(op, f));
        if let Terminator::Call { dest: Some(d), .. } = &mut block.term {
            f(d);
        }
    }
}

fn operand_place(op: &mut Operand, f: &mut impl FnMut(&mut Place)) {
    if let Operand::Copy(p) = op {
        f(p);
    }
}
