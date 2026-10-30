//! Whether every test in a loop is a counter's, so that none of them narrows a pointer the loop
//! reads through (the module doc's "no test of the loop narrows a type").

use std::collections::{HashMap, HashSet};

use velt_vir::vir::{
    AggLayout, BinOp, Function, Operand, Place, Rvalue, Stmt, Terminator, Ty, UnOp,
};

use super::Loop;
use crate::locals::place_ty;
use crate::visit::{rvalue_operands, term_operands};

/// Whether every `Branch` and `Switch` in `lp` tests a counter: a branch on a constant, or on
/// a comparison of two numbers (made in its block, perhaps negated or copied) one of which is a
/// local that changes from one iteration to the next, or a switch on such a local.
///
/// A value changes from one iteration to the next when it depends, through the assignments in
/// the loop, on a local whose value the loop computes from itself (`i = i + 1`). A type test
/// (`instanceof`, a union's tag, the result of one kept in a local) compares a value the loop
/// does not compute from itself with a constant, or joins tests (a `bool`), so it is not one.
pub(super) fn counted(aggs: &[AggLayout], func: &Function, lp: &Loop) -> bool {
    let changing = changing(func, lp);
    let changes = |op: &Operand| match op {
        Operand::Copy(p) if p.proj.is_empty() => changing.contains(&p.local.0),
        _ => false,
    };
    let number = |op: &Operand| {
        let ty = match op {
            Operand::Copy(p) => place_ty(aggs, func, p),
            Operand::Const(_, ty) => Some(*ty),
        };
        ty.is_some_and(|t| t.is_scalar() && !matches!(t, Ty::Bool | Ty::Ptr | Ty::V128))
    };
    func.blocks.iter().enumerate().all(|(bi, block)| {
        if !lp.body[bi] {
            return true;
        }
        match &block.term {
            Terminator::Branch { cond, .. } => {
                // Follow the condition back through its block: copies and negations of a
                // constant or of a comparison made there.
                let (mut cond, mut upto) = (cond, block.stmts.len());
                loop {
                    let c = match cond {
                        Operand::Const(..) => break true,
                        Operand::Copy(c) if c.proj.is_empty() => c,
                        Operand::Copy(_) => break false,
                    };
                    let Some(k) = block.stmts[..upto]
                        .iter()
                        .rposition(|s| matches!(s, Stmt::Assign(d, _) if d.local == c.local))
                    else {
                        break false;
                    };
                    let Stmt::Assign(d, rv) = &block.stmts[k] else {
                        unreachable!("ICE: rposition found an assignment");
                    };
                    if !d.proj.is_empty() {
                        break false;
                    }
                    match rv {
                        Rvalue::Use(o) | Rvalue::Unary(UnOp::Not, o) => (cond, upto) = (o, k),
                        Rvalue::Binary(op, a, b) => {
                            break matches!(
                                op,
                                BinOp::Eq
                                    | BinOp::Ne
                                    | BinOp::Lt
                                    | BinOp::Le
                                    | BinOp::Gt
                                    | BinOp::Ge
                            ) && number(a)
                                && number(b)
                                && (changes(a) || changes(b));
                        }
                        _ => break false,
                    }
                }
            }
            Terminator::Switch { value, .. } => changes(value) && number(value),
            Terminator::Goto(_)
            | Terminator::Call { .. }
            | Terminator::Return(_)
            | Terminator::Unreachable => true,
        }
    })
}

/// The locals the loop assigns that depend, through its assignments, on a local it computes
/// from itself.
fn changing(func: &Function, lp: &Loop) -> HashSet<u32> {
    // Per local the loop assigns: the locals its assignments there read.
    let mut deps: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut add = |dest: &Place, ops: Vec<&Operand>| {
        let list = deps.entry(dest.local.0).or_default();
        list.extend(ops.iter().filter_map(|op| match op {
            Operand::Copy(p) => Some(p.local.0),
            _ => None,
        }));
        // A partial assignment keeps the rest of the local.
        if !dest.proj.is_empty() {
            list.push(dest.local.0);
        }
    };
    for (bi, block) in func.blocks.iter().enumerate() {
        if !lp.body[bi] {
            continue;
        }
        for s in &block.stmts {
            if let Stmt::Assign(dest, rv) = s {
                let mut ops = vec![];
                rvalue_operands(rv, &mut |op| ops.push(op.clone()));
                add(dest, ops.iter().collect());
            }
        }
        if let Terminator::Call {
            dest: Some(dest), ..
        } = &block.term
        {
            let mut ops = vec![];
            term_operands(&block.term, &mut |op| ops.push(op.clone()));
            add(dest, ops.iter().collect());
        }
    }
    // Drop the locals none of whose reads is a local still in the graph, until none is left
    // to drop: those left reach a cycle (a local computed from itself).
    let keys: Vec<u32> = deps.keys().copied().collect();
    let mut outs: HashMap<u32, usize> = HashMap::new();
    let mut users: HashMap<u32, Vec<u32>> = HashMap::new();
    for &k in &keys {
        let mut n = 0;
        for &d in &deps[&k] {
            if deps.contains_key(&d) {
                n += 1;
                users.entry(d).or_default().push(k);
            }
        }
        outs.insert(k, n);
    }
    let mut alive: HashSet<u32> = keys.iter().copied().collect();
    let mut work: Vec<u32> = keys.iter().copied().filter(|k| outs[k] == 0).collect();
    while let Some(k) = work.pop() {
        if !alive.remove(&k) {
            continue;
        }
        for &u in users.get(&k).map_or(&[][..], |v| &v[..]) {
            if let Some(n) = outs.get_mut(&u) {
                *n -= 1;
                if *n == 0 {
                    work.push(u);
                }
            }
        }
    }
    alive
}
