//! Whether a comparison's bound is, by value, the bound the loop's head tests: then the head
//! already implies the comparison, LLVM removes it, and a copy of the loop would only add the
//! check in front (an ASCII scan to `min(count, bytes.length)` testing `bytes[i]`).
//!
//! Only ever a reason not to decide a test, so it may say "no" whenever it isn't sure.

use velt_vir::vir::{BinOp, Local, Operand, Proj, Rvalue, Stmt, Ty};

use super::{affine, invariant, Counter, Cx, Defs};

/// The index and bound a comparison the copy keeps tests, as `(base, k, n)` for `base + i + k`
/// against `n`, either way round; `None` for anything else.
pub(super) fn tested(
    cx: &Cx,
    defs: &Defs,
    counter: &Counter,
    st: &Stmt,
    at: (usize, usize),
) -> Option<(Option<Local>, u64, Operand)> {
    let Stmt::Assign(c, Rvalue::Binary(BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, x, y)) = st
    else {
        return None;
    };
    if !c.proj.is_empty() || cx.func.locals.get(c.local.0 as usize).map(|d| d.ty) != Some(Ty::Bool)
    {
        return None;
    }
    [(x, y), (y, x)].into_iter().find_map(|(index, n)| {
        let sum = affine(cx, defs, counter, index, at, 0)?;
        (sum.counter && invariant(cx, defs, n)).then(|| (sum.base, sum.k, n.clone()))
    })
}

/// Whether `n` holds the head's bound `bound` on every path: equal operands, or every value
/// `bound` may take (through copies) a read of the place `n` reads, in the same block.
pub(super) fn head_bound(cx: &Cx, all: &Defs, n: &Operand, bound: &Operand) -> bool {
    if n == bound {
        return true;
    }
    let (Operand::Copy(np), Operand::Copy(bp)) = (n, bound) else {
        return false;
    };
    if !np.proj.is_empty() || !bp.proj.is_empty() {
        return false;
    }
    let (mut ns, mut bs) = (vec![], vec![]);
    if !sources(cx, all, np.local, 0, &mut ns) || !sources(cx, all, bp.local, 0, &mut bs) {
        return false;
    }
    bs.iter()
        .all(|&b| ns.iter().any(|&m| same_read(cx, all, b, m)))
}

/// The locals at the ends of `l`'s copy chains: `l` itself unless every assignment of it copies
/// a whole local. False past a few steps.
fn sources(cx: &Cx, all: &Defs, l: Local, depth: u32, out: &mut Vec<Local>) -> bool {
    if depth > 8 {
        return false;
    }
    let Some(sites) = all.get(&l.0) else {
        out.push(l);
        return true;
    };
    let copies: Option<Vec<Local>> = (sites.iter())
        .map(|&(b, s)| match cx.func.blocks[b].stmts.get(s) {
            Some(Stmt::Assign(_, Rvalue::Use(Operand::Copy(p)))) if p.proj.is_empty() => {
                Some(p.local)
            }
            _ => None,
        })
        .collect();
    match copies {
        Some(srcs) => srcs
            .into_iter()
            .all(|x| sources(cx, all, x, depth + 1, out)),
        None => {
            out.push(l);
            true
        }
    }
}

/// Whether `a` and `b` are one local, or each assigned once, both by a read of the same field or
/// pointee (`h.1`, `(*p).1`), in the same block, with nothing between that writes the place's
/// local or through a pointer (a call ends a block).
fn same_read(cx: &Cx, all: &Defs, a: Local, b: Local) -> bool {
    if a == b {
        return true;
    }
    let read = |l: Local| {
        let [(blk, s)] = all.get(&l.0)?.as_slice() else {
            return None;
        };
        match cx.func.blocks[*blk].stmts.get(*s) {
            Some(Stmt::Assign(_, Rvalue::Use(Operand::Copy(p)))) if !p.proj.is_empty() => {
                Some((*blk, *s, p))
            }
            _ => None,
        }
    };
    let (Some((ba, sa, pa)), Some((bb, sb, pb))) = (read(a), read(b)) else {
        return false;
    };
    if ba != bb || pa != pb {
        return false;
    }
    // No assignment between the two reads writes through a pointer or to the pointer itself.
    let (lo, hi) = (sa.min(sb), sa.max(sb));
    cx.func.blocks[ba].stmts[lo + 1..hi]
        .iter()
        .all(|s| match s {
            Stmt::Assign(dest, _) => {
                dest.local != pa.local && !dest.proj.iter().any(|x| matches!(x, Proj::Deref(_)))
            }
            Stmt::Nop => true,
            _ => false,
        })
}
