//! Branch refinement for `numrep`: which comparison decides a branch, and what it proves about
//! the compared values on each edge.

use velt_vir::vir::{BinOp, Function, Local, Operand, Rvalue, Stmt, Terminator, Ty, UnOp};

use super::fact::{Fact, TWO_53};
use super::flow::is_comparison;

/// The comparison `lhs op rhs` deciding a branch; `negated` when the branch tests its negation.
pub(super) struct Condition {
    pub op: BinOp,
    pub lhs: Operand,
    pub rhs: Operand,
    pub negated: bool,
}

/// The last whole assignment to `l` among `stmts[..end]`.
fn last_def(stmts: &[Stmt], l: Local, end: usize) -> Option<(usize, &Rvalue)> {
    stmts[..end]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, s)| match s {
            Stmt::Assign(d, rv) if d.local == l => d.proj.is_empty().then_some((i, rv)),
            _ => None,
        })
}

fn assigned_in(stmts: &[Stmt], op: &Operand) -> bool {
    let Operand::Copy(p) = op else { return false };
    stmts
        .iter()
        .any(|s| matches!(s, Stmt::Assign(d, _) if d.local == p.local))
}

/// The comparison deciding block `b`'s branch, defined in the block (through copies and `!`),
/// with neither operand reassigned after it.
pub(super) fn condition(func: &Function, b: usize) -> Option<Condition> {
    let block = &func.blocks[b];
    let Terminator::Branch {
        cond: Operand::Copy(c),
        ..
    } = &block.term
    else {
        return None;
    };
    if !c.proj.is_empty() {
        return None;
    }
    let (mut local, mut end, mut negated) = (c.local, block.stmts.len(), false);
    for _ in 0..4 {
        let (at, rv) = last_def(&block.stmts, local, end)?;
        match rv {
            Rvalue::Use(Operand::Copy(p)) if p.proj.is_empty() => local = p.local,
            Rvalue::Unary(UnOp::Not, Operand::Copy(p)) if p.proj.is_empty() => {
                local = p.local;
                negated = !negated;
            }
            Rvalue::Binary(op, x, y) if is_comparison(*op) => {
                let later = &block.stmts[at + 1..];
                if assigned_in(later, x) || assigned_in(later, y) {
                    return None;
                }
                return Some(Condition {
                    op: *op,
                    lhs: x.clone(),
                    rhs: y.clone(),
                    negated,
                });
            }
            _ => return None,
        }
        // The copied local must still hold its value where the condition reads it.
        if block.stmts[at..end]
            .iter()
            .any(|s| matches!(s, Stmt::Assign(d, _) if d.local == local))
        {
            return None;
        }
        end = at;
    }
    None
}

/// How `op` was made from the local [`converted_from`] finds.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Conversion {
    /// A copy.
    Copy,
    /// An integer converted to `f64`, exact only within ±2^53.
    ToF64,
    /// An `f64` converted to this integer type (truncated, saturated, NaN gives 0), as an
    /// index (`xs[k]` with a whole `k`).
    FromF64(Ty),
}

/// The facts about the `f64` a conversion to `ty` gave the value `f`: it truncates, so the
/// double lies within 1 of `f`'s bounds (exclusive), where those are not the saturated ends;
/// it was NaN only if `f` holds 0.
pub(super) fn unconverted(f: Fact, ty: Ty) -> Fact {
    let top = Fact::top(ty);
    // Below 2^53, `x ± 1` is exact.
    let near = |x: f64| x.abs() < TWO_53;
    let lo = if f.lo > top.lo && near(f.lo) {
        (f.lo - 1.0).next_up()
    } else {
        f64::NEG_INFINITY
    };
    let hi = if f.hi < top.hi && near(f.hi) {
        (f.hi + 1.0).next_down()
    } else {
        f64::INFINITY
    };
    Fact {
        lo,
        hi,
        integral: false,
        nan: f.may_be_zero(),
        neg_zero: f.may_be_zero(),
    }
}

/// The local `op` copies or converts in block `b` (unchanged since, as is `op`): refining `op`
/// refines it too, through the conversion.
pub(super) fn converted_from(
    func: &Function,
    b: usize,
    op: &Operand,
    preds: &[Vec<usize>],
) -> Option<(Operand, Conversion)> {
    let Operand::Copy(p) = op else { return None };
    if !p.proj.is_empty() {
        return None;
    }
    let stmts = &func.blocks[b].stmts;
    let Some((at, rv)) = last_def(stmts, p.local, stmts.len()) else {
        return index_converted_from(func, b, p.local, preds);
    };
    let (src, cast) = match rv {
        Rvalue::Use(src @ Operand::Copy(z)) if z.proj.is_empty() => (src, Conversion::Copy),
        Rvalue::Cast(src @ Operand::Copy(z), Ty::F64)
            if z.proj.is_empty() && func.locals[z.local.0 as usize].ty.is_int() =>
        {
            (src, Conversion::ToF64)
        }
        Rvalue::Cast(src @ Operand::Copy(z), to)
            if z.proj.is_empty()
                && func.locals[z.local.0 as usize].ty == Ty::F64
                && to.is_int() =>
        {
            (src, Conversion::FromF64(*to))
        }
        _ => return None,
    };
    (!assigned_in(&stmts[at..], src)).then(|| (src.clone(), cast))
}

/// Blocks up the chain of single predecessors of `b` that [`index_converted_from`] searches.
const CHAIN: usize = 8;

/// The `f64` local that `l` (not assigned in block `b`) converts to an integer in a block up
/// the chain of single predecessors of `b`, possibly through copies, with none of them changed
/// since: an index `k as u64` (then `i = t`) tested against a length a few blocks later.
fn index_converted_from(
    func: &Function,
    b: usize,
    l: Local,
    preds: &[Vec<usize>],
) -> Option<(Operand, Conversion)> {
    // The chain of blocks, `b` first.
    let mut chain = vec![b];
    let mut want = l;
    // Locals read at (chain index, statement index) that must keep their value up to `b`'s end.
    let mut guards: Vec<(Local, usize, usize)> = vec![];
    let mut found = None;
    'walk: for _ in 0..CHAIN {
        let cur = *chain.last()?;
        let [p] = preds[cur][..] else { return None };
        if chain.contains(&p) {
            return None;
        }
        chain.push(p);
        let ci = chain.len() - 1;
        if matches!(&func.blocks[p].term, Terminator::Call { dest: Some(d), .. } if d.local == want)
        {
            return None;
        }
        let stmts = &func.blocks[p].stmts;
        let mut end = stmts.len();
        while let Some((at, rv)) = last_def(stmts, want, end) {
            match rv {
                Rvalue::Use(Operand::Copy(m)) if m.proj.is_empty() => {
                    guards.push((m.local, ci, at));
                    want = m.local;
                    end = at;
                }
                Rvalue::Cast(src @ Operand::Copy(z), to)
                    if z.proj.is_empty()
                        && func.locals[z.local.0 as usize].ty == Ty::F64
                        && to.is_int() =>
                {
                    guards.push((z.local, ci, at));
                    found = Some((src.clone(), Conversion::FromF64(*to)));
                    break 'walk;
                }
                _ => return None,
            }
        }
        // A copy's source may be assigned in this block before the copy: that is fine, the walk
        // continues with the source in the blocks above.
    }
    let found = found?;
    // Each guarded local keeps its value from where it is read to the end of `b`.
    let changes = |x: Local, s: &Stmt| matches!(s, Stmt::Assign(d, _) if d.local == x);
    for &(x, ci, at) in &guards {
        for (k, &blk) in chain.iter().enumerate().take(ci + 1) {
            let block = &func.blocks[blk];
            let from = if k == ci { at + 1 } else { 0 };
            if block.stmts[from..].iter().any(|s| changes(x, s)) {
                return None;
            }
            let call_sets = matches!(&block.term,
                Terminator::Call { dest: Some(d), .. } if d.local == x);
            if call_sets {
                return None;
            }
        }
    }
    Some(found)
}

/// The locals that hold a copy of `op` at the end of block `b` (`sum = t; branch t >= k`):
/// each was last set in the block to `op`'s value, and neither changed since. Refining `op`
/// refines them too.
pub(super) fn copies_of(func: &Function, b: usize, op: &Operand) -> Vec<Local> {
    let Operand::Copy(p) = op else { return vec![] };
    if !p.proj.is_empty() {
        return vec![];
    }
    let stmts = &func.blocks[b].stmts;
    let from = last_def(stmts, p.local, stmts.len()).map_or(0, |(at, _)| at + 1);
    let mut out = vec![];
    for (i, s) in stmts.iter().enumerate().skip(from) {
        let Stmt::Assign(d, Rvalue::Use(Operand::Copy(q))) = s else {
            continue;
        };
        let copy = d.proj.is_empty() && q.proj.is_empty() && q.local == p.local;
        let later = &stmts[i + 1..];
        let kept = !later
            .iter()
            .any(|s| matches!(s, Stmt::Assign(e, _) if e.local == d.local));
        if copy && kept && d.local != p.local {
            out.push(d.local);
        }
    }
    out
}

fn negate(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Ge,
        BinOp::Le => BinOp::Gt,
        BinOp::Gt => BinOp::Le,
        BinOp::Ge => BinOp::Lt,
        BinOp::Eq => BinOp::Ne,
        _ => BinOp::Eq,
    }
}

/// The facts about `x` and `y` given that `x op y` is `holds`; `None` if that is impossible.
pub(super) fn refine(op: BinOp, holds: bool, x: Fact, y: Fact) -> Option<(Fact, Fact)> {
    let rel = if holds {
        op
    } else if x.nan || y.nan {
        // A false comparison may just mean NaN.
        return Some((x, y));
    } else {
        negate(op)
    };
    if rel == BinOp::Ne {
        return Some((x, y));
    }
    // Every other relation that holds rules NaN out.
    let (mut x, mut y) = (Fact { nan: false, ..x }, Fact { nan: false, ..y });
    match rel {
        BinOp::Lt => {
            x.hi = x.hi.min(below(y.hi));
            y.lo = y.lo.max(above(x.lo));
        }
        BinOp::Le => {
            x.hi = x.hi.min(y.hi);
            y.lo = y.lo.max(x.lo);
        }
        BinOp::Gt | BinOp::Ge => {
            let flipped = if rel == BinOp::Gt {
                BinOp::Lt
            } else {
                BinOp::Le
            };
            let (ny, nx) = refine(flipped, true, y, x)?;
            return Some((nx, ny));
        }
        _ => {
            let (lo, hi) = (x.lo.max(y.lo), x.hi.min(y.hi));
            let integral = x.integral || y.integral;
            (x.lo, x.hi, x.integral) = (lo, hi, integral);
            (y.lo, y.hi, y.integral) = (lo, hi, integral);
        }
    }
    let (x, y) = (normalized(x), normalized(y));
    (!x.is_empty() && !y.is_empty()).then_some((x, y))
}

/// A bound for the values (doubles, or integers of an integer type) smaller than `v`: the next
/// double down where doubles are at most 1 apart, `v` itself beyond. Past ±2^53 an integer
/// below `v` may lie between `v` and the next double down (`2^60 - 1 < 2^60`, or
/// `-2^53 - 1 < -2^53`).
fn below(v: f64) -> f64 {
    if v > -TWO_53 && v <= TWO_53 {
        v.next_down()
    } else {
        v
    }
}

/// The mirror of [`below`]: a bound for the values greater than `v`.
fn above(v: f64) -> f64 {
    -below(-v)
}

/// Whole bounds for whole values, and no `-0` once 0 is ruled out.
fn normalized(mut f: Fact) -> Fact {
    if f.integral {
        f.lo = f.lo.ceil();
        f.hi = f.hi.floor();
    }
    if !f.may_be_zero() {
        f.neg_zero = false;
    }
    f
}
