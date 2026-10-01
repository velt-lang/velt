//! Which locals the range analysis tracks: the backward slice of the dividends (every
//! register-like integer local whose value can flow into one), plus the locals they are
//! compared with, whose bounds refine them at branches.

use velt_vir::vir::{BinOp, Function, Local, Operand, Rvalue, Stmt};

use crate::locals::Usage;
use crate::visit::rvalue_operands;

/// Register-like integer locals that can flow into `seeds` through assignments, plus the
/// locals they are compared with.
pub(super) fn slice(func: &Function, seeds: &[Local]) -> Vec<Local> {
    let usage = Usage::of(func);
    let int_reg = |l: Local| usage.is_register(l) && func.locals[l.0 as usize].ty.is_int();
    let n = func.locals.len();
    let mut inputs: Vec<Vec<Local>> = vec![vec![]; n];
    let mut compared: Vec<(Local, Local)> = vec![];
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        let Stmt::Assign(d, rv) = s else { continue };
        let mut locals = vec![];
        rvalue_operands(rv, &mut |op| {
            if let Operand::Copy(p) = op {
                if p.proj.is_empty() {
                    locals.push(p.local);
                }
            }
        });
        if let Rvalue::Binary(BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq, ..) = rv {
            if let [a, b] = locals[..] {
                compared.push((a, b));
            }
        } else if d.proj.is_empty() {
            inputs[d.local.0 as usize].extend(locals);
        }
    }
    let mut in_slice = vec![false; n];
    let mut work: Vec<Local> = seeds.iter().copied().filter(|&l| int_reg(l)).collect();
    while let Some(l) = work.pop() {
        if std::mem::replace(&mut in_slice[l.0 as usize], true) {
            continue;
        }
        let mut next: Vec<Local> = inputs[l.0 as usize].clone();
        next.extend(
            compared
                .iter()
                .filter_map(|&(a, b)| (a == l).then_some(b).or((b == l).then_some(a))),
        );
        work.extend(
            next.into_iter()
                .filter(|&x| int_reg(x) && !in_slice[x.0 as usize]),
        );
    }
    (0..n as u32)
        .map(Local)
        .filter(|l| in_slice[l.0 as usize])
        .collect()
}
