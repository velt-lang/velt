//! A comparison of an integer converted to a double with a double the facts know is a whole
//! number below 2^53 in magnitude, made in integers:
//!
//! ```text
//! t = cast i as f64        // i: u64 (or any integer type)
//! c = lt t, n              // n whole, never NaN, |n| < 2^53 (an array length, say)
//! ```
//!
//! becomes `c = lt i, n as u64` (`i64` for a signed `i`). It is exact for every `i`: below 2^53
//! both convert exactly; above it, `i as f64` is still at least 2^53 (rounding is monotone), so
//! it compares with `n` as `i` does, and likewise below -2^53. An unsigned `i` against an `n`
//! that may be negative (`len - 15`, `len - len % 64`, whose intervals reach below 0) tests the
//! sign of `n` too: `i < n` is `n > 0 & i < n as u64`, `i > n` is `n < 0 | i > n as u64`
//! (`refine::condition` reads these forms back as `i < n` and `i > n`, which they are exactly).
//!
//! The loop `for (let i: usize = 0; i < n; i++)` over `const n = xs.length` then tests its exit
//! in integers, which keeps its trip count computable (LLVM vectorizes it) and its bounds checks
//! provable.
//!
//! It is `numrep`'s last step, after narrowing: a comparison narrowing made in integers is left
//! as narrowing made it, and only those it left in doubles are rewritten here.

use velt_vir::vir::{BinOp, Const, Function, Local, LocalDecl, Operand, Place, Rvalue, Stmt, Ty};

use super::fact::TWO_53;
use super::flow::{is_comparison, operand_ty, Flow, State};
use super::Env;

/// Rewrite the comparisons of `func` that this module makes in integers; returns whether any
/// was rewritten. The facts come from a fresh analysis, made only when `func` has a candidate.
pub(super) fn run(env: &Env, func: &mut Function) -> bool {
    let Some(bounds) = candidates(func) else {
        return false;
    };
    // Facts for the bounds and what they are computed from only: a block of thousands of
    // conversions with one comparison costs that comparison's slice, not a second analysis of
    // every number in the function.
    let tracked = slice(func, bounds);
    let elems = vec![None; func.locals.len()];
    let Some(flow) = Flow::compute(func, env, &tracked, elems) else {
        return false;
    };
    let mut st: State = flow.scratch();
    let mut conv = Conversions::new(func);
    let mut planned = vec![];
    for bi in 0..func.blocks.len() {
        if !flow.load(bi, &mut st) {
            continue;
        }
        conv.begin_block();
        let mut plans = vec![];
        for (si, s) in func.blocks[bi].stmts.iter().enumerate() {
            if let Some(p) = plan(&flow, func, &st, &conv, si, s) {
                plans.push(p);
            }
            conv.note(func, s);
            flow.transfer(&mut st, func, s);
        }
        if !plans.is_empty() {
            planned.push((bi, plans));
        }
    }
    // After the walk: the new locals have no facts.
    let changed = !planned.is_empty();
    for (bi, plans) in planned {
        rewrite(func, bi, plans);
    }
    changed
}

/// The double locals some block compares with an integer it converted to `f64` before (the
/// bounds whose facts decide a rewrite), or `None` when no block has such a comparison. A
/// constant bound needs no facts; it makes the function a candidate with no local.
fn candidates(func: &Function) -> Option<Vec<Local>> {
    let mut found = false;
    let mut bounds = vec![];
    let mut converted = vec![false; func.locals.len()];
    let is_converted = |c: &[bool], op: &Operand| matches!(op, Operand::Copy(p) if p.proj.is_empty() && c[p.local.0 as usize]);
    for b in &func.blocks {
        let mut touched = vec![];
        for s in &b.stmts {
            let Stmt::Assign(d, rv) = s else { continue };
            match rv {
                Rvalue::Cast(src, Ty::F64)
                    if d.proj.is_empty() && operand_ty(func, src).is_int() =>
                {
                    converted[d.local.0 as usize] = true;
                    touched.push(d.local);
                }
                Rvalue::Binary(op, x, y)
                    if is_comparison(*op) && operand_ty(func, x) == Ty::F64 =>
                {
                    for (a, other) in [(x, y), (y, x)] {
                        if is_converted(&converted, a) {
                            found = true;
                            if let Operand::Copy(p) = other {
                                if p.proj.is_empty() {
                                    bounds.push(p.local);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for l in touched {
            converted[l.0 as usize] = false;
        }
    }
    found.then_some(bounds)
}

/// `bounds` and the numeric register locals their values are computed from (through every
/// assignment to them, transitively): the locals whose facts the plan reads.
fn slice(func: &Function, bounds: Vec<Local>) -> Vec<Local> {
    let usage = crate::locals::Usage::of(func);
    let n = func.locals.len();
    let mut defs: Vec<Vec<Local>> = vec![vec![]; n];
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        let Stmt::Assign(d, rv) = s else { continue };
        if !d.proj.is_empty() {
            continue;
        }
        crate::visit::rvalue_operands(rv, &mut |op| {
            if let Operand::Copy(p) = op {
                if p.proj.is_empty() {
                    defs[d.local.0 as usize].push(p.local);
                }
            }
        });
    }
    let wanted = |l: Local| usage.is_register(l) && super::numeric(func.locals[l.0 as usize].ty);
    let mut seen = vec![false; n];
    let mut tracked = vec![];
    let mut work: Vec<Local> = bounds.into_iter().filter(|l| wanted(*l)).collect();
    while let Some(l) = work.pop() {
        if std::mem::replace(&mut seen[l.0 as usize], true) {
            continue;
        }
        tracked.push(l);
        work.extend(defs[l.0 as usize].iter().copied().filter(|l| wanted(*l)));
    }
    tracked.sort_by_key(|l| l.0);
    tracked
}

/// A comparison at statement `si` that `rewrite` makes in integers: which operand is the
/// converted integer (`left`), the integer it was converted from, and the type to compare in.
struct Plan {
    si: usize,
    left: bool,
    int: Operand,
    ty: Ty,
    /// An unsigned integer against a double that may be negative: the sign is tested too.
    signed_bound: bool,
}

/// The integer conversions to `f64` made so far in the current block, each looked up and
/// invalidated in constant time (a block of thousands of conversions stays linear, #1658):
/// per double local, the integer local it was converted from, the block (`epoch`) and that
/// integer's version then. A write to a local bumps its version; a store through a pointer or a
/// memory operation, which may reach an address-taken local, starts a new epoch.
struct Conversions {
    source: Vec<Option<(Local, u32, u32)>>,
    version: Vec<u32>,
    epoch: u32,
}

impl Conversions {
    fn new(func: &Function) -> Conversions {
        Conversions {
            source: vec![None; func.locals.len()],
            version: vec![0; func.locals.len()],
            epoch: 0,
        }
    }

    fn begin_block(&mut self) {
        self.epoch += 1;
    }

    /// Note statement `s` (after `plan` looked at it).
    fn note(&mut self, func: &Function, s: &Stmt) {
        match s {
            Stmt::Assign(p, rv) if p.proj.is_empty() => {
                let d = p.local.0 as usize;
                self.version[d] += 1;
                self.source[d] = match rv {
                    Rvalue::Cast(Operand::Copy(sp), Ty::F64)
                        if sp.proj.is_empty()
                            && func.locals[sp.local.0 as usize].ty.is_int()
                            && func.locals[d].ty == Ty::F64 =>
                    {
                        let v = self.version[sp.local.0 as usize];
                        Some((sp.local, self.epoch, v))
                    }
                    _ => None,
                };
            }
            // A store through a pointer may reach an address-taken local.
            Stmt::Assign(..) | Stmt::MemCopy { .. } | Stmt::MemCopyDyn { .. } => self.epoch += 1,
            Stmt::MemSet { .. } => self.epoch += 1,
            Stmt::Nop => {}
        }
    }

    /// The integer `op` (a double local) was converted from in this block, if neither has
    /// changed since.
    fn source(&self, op: &Operand) -> Option<Operand> {
        let Operand::Copy(p) = op else { return None };
        if !p.proj.is_empty() {
            return None;
        }
        let (src, epoch, v) = self.source[p.local.0 as usize]?;
        (epoch == self.epoch && self.version[src.0 as usize] == v)
            .then(|| Operand::Copy(Place::local(src)))
    }
}

/// Whether statement `si` (`s`) is such a comparison, given the facts `st` before it.
fn plan(
    flow: &Flow,
    func: &Function,
    st: &State,
    conv: &Conversions,
    si: usize,
    s: &Stmt,
) -> Option<Plan> {
    let Stmt::Assign(_, Rvalue::Binary(op, a, b)) = s else {
        return None;
    };
    if !is_comparison(*op) || operand_ty(func, a) != Ty::F64 || operand_ty(func, b) != Ty::F64 {
        return None;
    }
    // Each condition the exactness argument needs, spelled out rather than through
    // `Fact::exact_int`: whole, never NaN, a non-empty interval strictly within ±2^53.
    let bounded = |other: &Operand| {
        flow.operand(st, func, other)
            .filter(|y| y.integral && !y.nan && y.lo <= y.hi && y.lo > -TWO_53 && y.hi < TWO_53)
    };
    // Either operand may be the converted integer, and the other one the bounded double.
    let (left, int, y) = [(true, a, b), (false, b, a)]
        .into_iter()
        .find_map(|(left, x, other)| Some((left, conv.source(x)?, bounded(other)?)))?;
    let from = operand_ty(func, &int);
    let ty = if from.is_signed() { Ty::I64 } else { Ty::U64 };
    let signed_bound = !from.is_signed() && y.lo < 0.0;
    // `x == n` and `x != n` with a negative `n` need no rewrite to be cheap: leave them.
    if signed_bound && matches!(op, BinOp::Eq | BinOp::Ne) {
        return None;
    }
    Some(Plan {
        si,
        left,
        int,
        ty,
        signed_bound,
    })
}

/// Rewrite the comparisons `plans` (in statement order) of block `bi`.
fn rewrite(func: &mut Function, bi: usize, plans: Vec<Plan>) {
    let fresh = |func: &mut Function, ty| {
        func.locals.push(LocalDecl::new(ty, None));
        Local(func.locals.len() as u32 - 1)
    };
    let mut temps = vec![];
    for p in &plans {
        let from = operand_ty(func, &p.int);
        let x = (from != p.ty).then(|| fresh(func, p.ty));
        let y = fresh(func, if p.signed_bound { Ty::I64 } else { p.ty });
        let signed = p.signed_bound.then(|| {
            [
                fresh(func, Ty::U64),
                fresh(func, Ty::Bool),
                fresh(func, Ty::Bool),
            ]
        });
        temps.push((x, y, signed));
    }
    let mut si = 0;
    let mut next = 0;
    crate::srclocs::rewrite_stmts(func, bi, |s, out| {
        let i = si;
        si += 1;
        let Some(p) = plans.get(next).filter(|p| p.si == i) else {
            out.push(s);
            return;
        };
        let (x_t, y_t, signed) = temps[next];
        next += 1;
        let Stmt::Assign(d, Rvalue::Binary(op, a, b)) = s else {
            unreachable!("ICE: a planned comparison is a comparison");
        };
        let (op, other) = match p.left {
            true => (op, b),
            false => (flipped(op), a),
        };
        let x = match x_t {
            Some(t) => {
                out.push(Stmt::Assign(
                    Place::local(t),
                    Rvalue::Cast(p.int.clone(), p.ty),
                ));
                Operand::Copy(Place::local(t))
            }
            None => p.int.clone(),
        };
        let y_ty = if p.signed_bound { Ty::I64 } else { p.ty };
        out.push(Stmt::Assign(Place::local(y_t), Rvalue::Cast(other, y_ty)));
        let y = Operand::Copy(Place::local(y_t));
        let Some([yu, cmp, sign]) = signed else {
            out.push(Stmt::Assign(d, Rvalue::Binary(op, x, y)));
            return;
        };
        // `x op y` for an unsigned `x` and a signed `y`: `y` past 0 on the side that decides,
        // then `x op (y as u64)`. `refine::signed_bound` reads this shape back.
        let at = |l| Operand::Copy(Place::local(l));
        out.push(Stmt::Assign(
            Place::local(yu),
            Rvalue::Cast(y.clone(), Ty::U64),
        ));
        out.push(Stmt::Assign(
            Place::local(cmp),
            Rvalue::Binary(op, x, at(yu)),
        ));
        let zero = Operand::Const(Const::Int(0), Ty::I64);
        let (test, join) = sign_test(op);
        out.push(Stmt::Assign(
            Place::local(sign),
            Rvalue::Binary(test, y, zero),
        ));
        out.push(Stmt::Assign(d, Rvalue::Binary(join, at(sign), at(cmp))));
    });
}

/// For `x op y` with an unsigned `x` and a signed `y`: the test of `y` against 0 and how it
/// joins `x op (y as u64)`. `<` and `<=` need `y` past 0 (`&`); `>` and `>=` hold for any
/// negative `y` (`|`).
pub(super) fn sign_test(op: BinOp) -> (BinOp, BinOp) {
    match op {
        BinOp::Lt => (BinOp::Gt, BinOp::BitAnd),
        BinOp::Le => (BinOp::Ge, BinOp::BitAnd),
        _ => (BinOp::Lt, BinOp::BitOr), // Gt, Ge
    }
}

/// `op` with its operands swapped: `a < b` is `b > a`.
fn flipped(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Le => BinOp::Ge,
        BinOp::Gt => BinOp::Lt,
        BinOp::Ge => BinOp::Le,
        op => op,
    }
}
