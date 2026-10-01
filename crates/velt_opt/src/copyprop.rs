//! Copy propagation over register-like locals, in two flavours:
//! - **global**: `a = b` where `a` is assigned only there and `b` is assigned at most once
//!   (or is an unmodified param). "Assigned before read" means `a`'s single definition
//!   dominates its reads and `b`'s dominates `a`'s, so no path can redefine `b` between them
//!   without also re-running `a = b`: every read of `a` may read `b` instead.
//! - **block-local**: within a block, reads of `a` after `a = b` use `b` until either is
//!   reassigned (covers mutable variables and inlined parameters).
//!
//! The copies themselves become dead and are removed by `dce`.

use velt_vir::vir::{Function, Local, Operand, Place, Rvalue, Stmt};

use crate::locals::{as_register, Usage};
use crate::visit::{stmt_operands_mut, term_operands_mut};

/// Propagate copies in `func`; returns whether any read was rewritten.
pub(crate) fn run(func: &mut Function) -> bool {
    let usage = Usage::of(func);
    let mut changed = global(func, &usage);
    changed |= block_local(func, &usage);
    changed
}

/// Whether `s` is `a = b` between distinct register locals of the same type.
fn as_copy(func: &Function, usage: &Usage, s: &Stmt) -> Option<(Local, Local)> {
    let Stmt::Assign(dst, Rvalue::Use(Operand::Copy(src))) = s else {
        return None;
    };
    let (a, b) = (as_register(usage, dst)?, as_register(usage, src)?);
    let same_ty = func.locals[a.0 as usize].ty == func.locals[b.0 as usize].ty;
    (a != b && same_ty).then_some((a, b))
}

fn global(func: &mut Function, usage: &Usage) -> bool {
    let n = func.locals.len();
    let mut replacement: Vec<Option<Local>> = vec![None; n];
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        if let Some((a, b)) = as_copy(func, usage, s) {
            let a_single = a.0 as usize >= func.params.len() && usage.get(a).defs == 1;
            if a_single && usage.get(b).defs <= 1 {
                replacement[a.0 as usize] = Some(b);
            }
        }
    }
    // Resolve chains (`c = a; a = b` → c reads b). Chains are acyclic under "assigned before
    // read"; the step bound keeps malformed input from looping.
    let resolve = |mut l: Local| {
        for _ in 0..n {
            match replacement[l.0 as usize] {
                Some(next) => l = next,
                None => break,
            }
        }
        l
    };
    let resolved: Vec<Option<Local>> = (0..n)
        .map(|i| replacement[i].map(|_| resolve(Local(i as u32))))
        .collect();
    if resolved.iter().all(Option::is_none) {
        return false;
    }
    let mut changed = false;
    let mut rename = |op: &mut Operand| {
        if let Operand::Copy(p) = op {
            if let Some(to) = resolved[p.local.0 as usize] {
                p.local = to;
                changed = true;
            }
        }
    };
    for block in &mut func.blocks {
        for s in &mut block.stmts {
            stmt_operands_mut(s, &mut rename);
            rename_deref_dest(s, &resolved);
        }
        term_operands_mut(&mut block.term, &mut rename);
    }
    changed
}

/// A destination that writes through a pointer reads the pointer local: rename it too.
fn rename_deref_dest(s: &mut Stmt, resolved: &[Option<Local>]) {
    if let Stmt::Assign(p, rv) = s {
        rename_deref_place(p, resolved);
        if let Rvalue::AddrOf(q) = rv {
            rename_deref_place(q, resolved);
        }
    }
}

fn rename_deref_place(p: &mut Place, resolved: &[Option<Local>]) {
    if crate::visit::derefs(p) {
        if let Some(to) = resolved[p.local.0 as usize] {
            p.local = to;
        }
    }
}

fn block_local(func: &mut Function, usage: &Usage) -> bool {
    let n = func.locals.len();
    let mut changed = false;
    // copy_of[a] = b while `a == b` is known to hold at this point of the block; `active`
    // lists the set entries, which are cleared after each block (allocating the table per
    // block was quadratic in long functions).
    let mut copy_of: Vec<Option<Local>> = vec![None; n];
    let mut active: Vec<Local> = Vec::new();
    for bi in 0..func.blocks.len() {
        for a in active.drain(..) {
            copy_of[a.0 as usize] = None;
        }
        for si in 0..func.blocks[bi].stmts.len() {
            let mut rename = |op: &mut Operand| {
                if let Operand::Copy(p) = op {
                    if let Some(to) = copy_of[p.local.0 as usize] {
                        p.local = to;
                        changed = true;
                    }
                }
            };
            let s = &mut func.blocks[bi].stmts[si];
            stmt_operands_mut(s, &mut rename);
            rename_deref_dest(s, &copy_of);
            let Stmt::Assign(dst, _) = s else { continue };
            let Some(d) = as_register(usage, dst) else {
                continue;
            };
            // `d` changed: forget facts about it on either side.
            active.retain(|&a| {
                let stale = a == d || copy_of[a.0 as usize] == Some(d);
                if stale {
                    copy_of[a.0 as usize] = None;
                }
                !stale
            });
            if let Some((a, b)) = as_copy(func, usage, &func.blocks[bi].stmts[si]) {
                copy_of[a.0 as usize] = Some(b);
                active.push(a);
            }
        }
        let mut rename = |op: &mut Operand| {
            if let Operand::Copy(p) = op {
                if let Some(to) = copy_of[p.local.0 as usize] {
                    p.local = to;
                    changed = true;
                }
            }
        };
        term_operands_mut(&mut func.blocks[bi].term, &mut rename);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use velt_vir::vir::{BinOp, Terminator, Ty};

    #[test]
    fn global_single_def_copy() {
        // bb0: t = p; goto bb1; bb1: r = t + t; return r
        let mut fb = FuncBuilder::internal("f", &[Ty::I64], Ty::I64);
        let p = fb.param(0);
        let (t, r) = (fb.local(Ty::I64), fb.local(Ty::I64));
        let (b0, b1) = (fb.block(), fb.block());
        fb.assign(b0, t, Rvalue::Use(copy_local(p)));
        fb.goto(b0, b1);
        fb.assign(b1, r, bin(BinOp::Add, copy_local(t), copy_local(t)));
        fb.ret(b1, copy_local(r));
        let mut f = fb.finish();
        assert!(run(&mut f));
        assert_eq!(
            f.blocks[1].stmts[0],
            Stmt::Assign(
                Place::local(r),
                bin(BinOp::Add, copy_local(p), copy_local(p))
            )
        );
    }

    #[test]
    fn block_local_copy_is_invalidated_by_redefinition() {
        // x = p; y = x; x = 5; z = y + x; return z   (y must still be p, x must be 5-var)
        let mut fb = FuncBuilder::internal("f", &[Ty::I64], Ty::I64);
        let p = fb.param(0);
        let (x, y, z) = (fb.local(Ty::I64), fb.local(Ty::I64), fb.local(Ty::I64));
        let b = fb.block();
        fb.assign(b, x, Rvalue::Use(copy_local(p)));
        fb.assign(b, y, Rvalue::Use(copy_local(x)));
        fb.assign(b, x, Rvalue::Use(int(5, Ty::I64)));
        fb.assign(b, z, bin(BinOp::Add, copy_local(y), copy_local(x)));
        fb.ret(b, copy_local(z));
        let mut f = fb.finish();
        assert!(run(&mut f));
        assert_eq!(
            f.blocks[0].stmts[3],
            Stmt::Assign(
                Place::local(z),
                bin(BinOp::Add, copy_local(p), copy_local(x))
            )
        );
    }

    #[test]
    fn source_redefinition_kills_the_copy() {
        // y = p; p = 0; return y   — `p` is reassigned, so reads of y must not become p.
        let mut fb = FuncBuilder::internal("f", &[Ty::I64], Ty::I64);
        let p = fb.param(0);
        let y = fb.local(Ty::I64);
        let b = fb.block();
        fb.assign(b, y, Rvalue::Use(copy_local(p)));
        fb.assign(b, p, Rvalue::Use(int(0, Ty::I64)));
        fb.ret(b, copy_local(y));
        let mut f = fb.finish();
        run(&mut f);
        assert_eq!(f.blocks[0].term, Terminator::Return(copy_local(y)));
    }

    #[test]
    fn address_taken_locals_are_left_alone() {
        let mut fb = FuncBuilder::internal("f", &[Ty::I64], Ty::I64);
        let p = fb.param(0);
        let (x, q) = (fb.local(Ty::I64), fb.local(Ty::Ptr));
        let b = fb.block();
        fb.assign(b, x, Rvalue::Use(copy_local(p)));
        fb.assign(b, q, Rvalue::AddrOf(Place::local(x)));
        fb.ret(b, copy_local(x));
        let mut f = fb.finish();
        assert!(!run(&mut f));
    }
}
