//! The abstract values of `bounds`: values, facts, conditions, a program point's state, and
//! the proofs and joins over them.

use rustc_hash::FxHashMap;
use velt_vir::vir::{BinOp, Function, Ty};

/// Arrivals at a join block after which its intervals are widened.
pub(super) const WIDEN_AFTER: u32 = 2;
/// Most facts kept per program point (the oldest are dropped beyond it).
pub(super) const MAX_FACTS: usize = 64;

/// `base + k` for `k` in `[lo, hi]` (modulo 2^64), or exactly `[lo, hi]` without a base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Val {
    pub(super) base: Option<u32>,
    pub(super) lo: i128,
    pub(super) hi: i128,
}

/// `x + k <= y` (mathematical); `x` `None` is 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Fact {
    pub(super) x: Option<u32>,
    pub(super) k: i128,
    pub(super) y: u32,
}

/// `c = a op b`, a comparison, as the operands were when it ran: unsigned, or (`signed`) a
/// signed one between a non-negative constant and an unsigned value cast to a signed type,
/// with the cast's source in its place.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Cond {
    pub(super) c: u32,
    pub(super) op: BinOp,
    pub(super) a: Term,
    pub(super) b: Term,
    pub(super) signed: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct State {
    pub(super) vals: FxHashMap<u32, Val>,
    pub(super) facts: Vec<Fact>,
    pub(super) conds: Vec<Cond>,
    /// `(v, u)`: signed `v` holds unsigned `u` cast (`xs.length` compared with a literal).
    pub(super) casts: Vec<(u32, u32)>,
}

impl State {
    /// Facts, conditions and casts in a fixed order, so that equal states compare equal.
    pub(super) fn normalize(&mut self) {
        self.facts.sort_by_key(|f| (f.x, f.y, f.k));
        self.conds.sort_by_key(|c| c.c);
        self.casts.sort_unstable();
    }
}

pub(super) const U64_MAX: i128 = u64::MAX as i128;

pub(super) fn unsigned(ty: Ty) -> bool {
    matches!(ty, Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64)
}

pub(super) fn max_of(ty: Ty) -> i128 {
    match ty {
        Ty::U8 => u8::MAX as i128,
        Ty::U16 => u16::MAX as i128,
        Ty::U32 => u32::MAX as i128,
        _ => U64_MAX,
    }
}

/// An operand of a comparison: a local (or a frozen field's term, numbered after the locals),
/// or a constant (its value in its type).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Term {
    Local(u32),
    Const(i128),
}

pub(super) fn mentions(t: &Term, v: u32) -> bool {
    matches!(t, Term::Local(l) if *l == v)
}

/// The value of a term.
pub(super) fn val_of_term(st: &State, t: Term) -> Val {
    match t {
        Term::Local(l) => val_of_local(st, l),
        Term::Const(c) => Val {
            base: None,
            lo: c,
            hi: c,
        },
    }
}

/// Forget local `v`: its value, the values based on it, the facts and conditions naming it.
pub(super) fn kill(st: &mut State, v: u32) {
    st.vals.remove(&v);
    st.vals.retain(|_, x| x.base != Some(v));
    st.facts.retain(|f| f.x != Some(v) && f.y != v);
    st.conds
        .retain(|c| c.c != v && !mentions(&c.a, v) && !mentions(&c.b, v));
    st.casts.retain(|(s, u)| *s != v && *u != v);
}

pub(super) fn add_fact(st: &mut State, f: Fact) {
    if let Some(old) = st.facts.iter_mut().find(|g| g.x == f.x && g.y == f.y) {
        old.k = old.k.max(f.k);
        return;
    }
    if st.facts.len() >= MAX_FACTS {
        st.facts.remove(0);
    }
    st.facts.push(f);
}

pub(super) fn val_of_local(st: &State, l: u32) -> Val {
    st.vals.get(&l).copied().unwrap_or(Val {
        base: Some(l),
        lo: 0,
        hi: 0,
    })
}

/// The largest `K` with `t >= K` known.
pub(super) fn lower_bound(st: &State, t: u32) -> i128 {
    st.facts
        .iter()
        .filter(|f| f.y == t)
        .map(|f| f.k)
        .max()
        .unwrap_or(0)
        .max(0)
}

/// Whether `p + extra <= q` is proven (mathematically, and so without any wrap).
///
/// A side with a base is `base + k` modulo 2^64. It is the mathematical sum when no offset
/// is negative on the left (the left side's partial sums are bounded by the right side's,
/// below 2^64) and no offset is positive on the right (it cannot wrap upward), with the
/// right side's base large enough that its negative offset does not wrap downward.
pub(super) fn prove_le(st: &State, p: Val, extra: i128, q: Val) -> bool {
    let ph = p.hi + extra;
    match (p.base, q.base) {
        (None, None) => ph <= q.lo,
        (_, Some(_)) if q.hi > 0 => false,
        (None, Some(b)) => {
            // `q >= b + q.lo` with `b >= K`.
            let k = lower_bound(st, b);
            k + q.lo >= ph && k + q.lo >= 0
        }
        (Some(a), Some(b)) if a == b => {
            // Both sides on one base: neither wraps if the base has room above it.
            p.lo >= 0 && ph <= q.lo && st.facts.iter().any(|f| f.x == Some(a) && f.k >= ph.max(0))
        }
        (Some(a), Some(b)) => {
            p.lo >= 0
                && st
                    .facts
                    .iter()
                    .any(|f| f.x == Some(a) && f.y == b && ph - q.lo <= f.k && f.k + q.lo >= 0)
        }
        (Some(_), None) => false,
    }
}

pub(super) fn meet(a: &State, b: &State) -> State {
    let mut vals = FxHashMap::default();
    for (l, x) in &a.vals {
        if let Some(y) = b.vals.get(l) {
            if x.base == y.base {
                vals.insert(
                    *l,
                    Val {
                        base: x.base,
                        lo: x.lo.min(y.lo),
                        hi: x.hi.max(y.hi),
                    },
                );
            }
        }
    }
    let facts = a
        .facts
        .iter()
        .filter_map(|f| {
            let g = b.facts.iter().find(|g| g.x == f.x && g.y == f.y)?;
            Some(Fact {
                k: f.k.min(g.k),
                ..*f
            })
        })
        .collect();
    let conds = a
        .conds
        .iter()
        .filter(|c| b.conds.contains(c))
        .cloned()
        .collect();
    let casts = a
        .casts
        .iter()
        .filter(|c| b.casts.contains(c))
        .copied()
        .collect();
    let mut st = State {
        vals,
        facts,
        conds,
        casts,
    };
    st.normalize();
    st
}

/// `new` (a join of `old` and an arrival) with every bound that moved pushed to its limit:
/// intervals to the type's range, facts dropped.
pub(super) fn widen(old: &State, new: &State, func: &Function) -> State {
    let mut vals = FxHashMap::default();
    for (l, x) in &new.vals {
        let Some(o) = old.vals.get(l) else { continue };
        if o.base != x.base {
            continue;
        }
        let ty = func.locals[*l as usize].ty;
        let lo = if x.lo < o.lo {
            if x.base.is_none() {
                0
            } else {
                continue;
            }
        } else {
            x.lo
        };
        let hi = if x.hi > o.hi {
            if x.base.is_none() {
                max_of(ty)
            } else {
                continue;
            }
        } else {
            x.hi
        };
        vals.insert(
            *l,
            Val {
                base: x.base,
                lo,
                hi,
            },
        );
    }
    let facts = new
        .facts
        .iter()
        .filter(|f| old.facts.contains(f))
        .copied()
        .collect();
    State {
        vals,
        facts,
        conds: new.conds.clone(),
        casts: new.casts.clone(),
    }
}
