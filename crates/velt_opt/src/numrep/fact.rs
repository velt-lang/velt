//! The facts `numrep` keeps about a numeric value, and how operations transform them.
//!
//! A [`Fact`] over-approximates the set of values a local may hold: an interval of its non-NaN
//! values (`-0` counts as 0 there), whether every non-NaN value is a whole number (or ±∞),
//! whether it may be NaN and whether it may be `-0`. Integer values are whole and never NaN or
//! `-0`. Float endpoints are computed in `f64`: IEEE round-to-nearest is monotone, so the
//! rounded result of an operation always lies between the rounded results at the corners.

use velt_vir::vir::{BinOp, Const, Ty, UnOp};

use crate::divisions::Interval;

/// 2^53: below it in magnitude, every whole number is a double and integer arithmetic on
/// doubles is exact.
pub(super) const TWO_53: f64 = 9_007_199_254_740_992.0;

/// What is known about one numeric value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Fact {
    /// Smallest non-NaN value (`-∞` allowed).
    pub lo: f64,
    /// Largest non-NaN value (`+∞` allowed).
    pub hi: f64,
    /// Every non-NaN value is a whole number or ±∞.
    pub integral: bool,
    /// May be NaN.
    pub nan: bool,
    /// May be `-0` (then the interval contains 0).
    pub neg_zero: bool,
}

impl Fact {
    /// Every value of type `ty`.
    pub fn top(ty: Ty) -> Fact {
        if ty.is_float() {
            return Fact {
                lo: f64::NEG_INFINITY,
                hi: f64::INFINITY,
                integral: false,
                nan: true,
                neg_zero: true,
            };
        }
        let r = Interval::full(if ty.is_int() { ty } else { Ty::U8 });
        let hi = if ty == Ty::Bool { 1 } else { r.hi };
        Fact::int(r.lo, hi)
    }

    /// No value at all (the join of nothing).
    pub fn empty() -> Fact {
        Fact {
            lo: f64::INFINITY,
            hi: f64::NEG_INFINITY,
            integral: true,
            nan: false,
            neg_zero: false,
        }
    }

    /// The whole numbers in `[lo, hi]`.
    pub fn int(lo: i128, hi: i128) -> Fact {
        Fact {
            lo: down(lo),
            hi: up(hi),
            integral: true,
            nan: false,
            neg_zero: false,
        }
    }

    /// The single value `x`.
    pub fn float(x: f64) -> Fact {
        if x.is_nan() {
            return Fact {
                lo: f64::INFINITY,
                hi: f64::NEG_INFINITY,
                integral: true,
                nan: true,
                neg_zero: false,
            };
        }
        Fact {
            lo: x,
            hi: x,
            integral: x.fract() == 0.0 || x.is_infinite(),
            nan: false,
            neg_zero: x == 0.0 && x.is_sign_negative(),
        }
    }

    /// The constant `c` of type `ty`, if numeric.
    pub fn of_const(c: &Const, ty: Ty) -> Option<Fact> {
        match (c, ty) {
            (Const::Float(x), Ty::F64) => Some(Fact::float(*x)),
            (Const::Int(v), t) if t.is_float() => Some(Fact::float(*v as f64)),
            (Const::Int(v), t) if t.is_int() => {
                let v = crate::constfold::normalize(*v, t);
                Some(Fact::int(v, v))
            }
            (Const::Bool(b), Ty::Bool) => Some(Fact::int(i128::from(*b), i128::from(*b))),
            _ => None,
        }
    }

    /// No value at all (an empty interval, not NaN).
    pub fn is_empty(&self) -> bool {
        self.lo > self.hi && !self.nan
    }

    /// The smallest fact holding both.
    pub fn join(self, o: Fact) -> Fact {
        Fact {
            lo: self.lo.min(o.lo),
            hi: self.hi.max(o.hi),
            integral: self.integral && o.integral,
            nan: self.nan || o.nan,
            neg_zero: self.neg_zero || o.neg_zero,
        }
    }

    /// May the value be zero (of either sign)?
    pub fn may_be_zero(&self) -> bool {
        self.lo <= 0.0 && self.hi >= 0.0
    }

    /// Largest magnitude of a non-NaN value.
    pub fn magnitude(&self) -> f64 {
        self.lo.abs().max(self.hi.abs())
    }

    /// Whole, never NaN and strictly within ±2^53: integer arithmetic on it is exact, and so is
    /// converting it to an `i64`.
    pub fn exact_int(&self) -> bool {
        self.integral && !self.nan && self.magnitude() < TWO_53
    }

    /// Within the range of the integer type `ty` (and whole, never NaN).
    pub fn fits(&self, ty: Ty) -> bool {
        let r = Interval::full(ty);
        self.integral && !self.nan && self.lo >= r.lo as f64 && self.hi <= r.hi as f64
    }

    /// As an integer interval (whole, finite values only).
    fn interval(&self) -> Option<Interval> {
        let finite = self.lo.is_finite() && self.hi.is_finite();
        (self.integral && !self.nan && finite && self.lo <= self.hi).then(|| Interval {
            lo: self.lo.floor() as i128,
            hi: self.hi.ceil() as i128,
        })
    }
}

/// `v` rounded down to a double.
fn down(v: i128) -> f64 {
    let x = v as f64;
    if x as i128 > v {
        x.next_down()
    } else {
        x
    }
}

/// `v` rounded up to a double.
fn up(v: i128) -> f64 {
    let x = v as f64;
    if (x as i128) < v {
        x.next_up()
    } else {
        x
    }
}

/// Facts about `op a` for an operand of type `ty`.
pub(super) fn unary(op: UnOp, ty: Ty, a: Fact) -> Fact {
    match op {
        UnOp::Neg if ty == Ty::F64 => Fact {
            lo: -a.hi,
            hi: -a.lo,
            integral: a.integral,
            nan: a.nan,
            neg_zero: a.may_be_zero(),
        },
        UnOp::Neg if ty.is_int() => match a.interval() {
            Some(i) => from_interval(
                Interval {
                    lo: -i.hi,
                    hi: -i.lo,
                },
                ty,
            ),
            None => Fact::top(ty),
        },
        _ => Fact::top(ty),
    }
}

/// Facts about `a op b` for operands of type `ty` (comparisons give `Bool`).
pub(super) fn binary(op: BinOp, ty: Ty, a: Fact, b: Fact) -> Fact {
    use BinOp::*;
    match op {
        Eq | Ne | Lt | Le | Gt | Ge => Fact::top(Ty::Bool),
        _ if ty == Ty::F64 => float_binary(op, a, b),
        _ if ty.is_int() => match (a.interval(), b.interval()) {
            (Some(x), Some(y)) => match crate::divisions::interval_binary(op, x, y, ty) {
                Some(i) => from_interval(i, ty),
                None => Fact::top(ty),
            },
            _ => Fact::top(ty),
        },
        _ => Fact::top(ty),
    }
}

/// The values of `i` as type `ty`, which wraps: the whole type when `i` does not fit.
fn from_interval(i: Interval, ty: Ty) -> Fact {
    let f = Interval::full(ty);
    if i.lo >= f.lo && i.hi <= f.hi {
        Fact::int(i.lo, i.hi)
    } else {
        Fact::int(f.lo, f.hi)
    }
}

fn float_binary(op: BinOp, a: Fact, b: Fact) -> Fact {
    // An operand without number values is NaN (or unreachable): so is the result.
    if a.lo > a.hi || b.lo > b.hi {
        return Fact::float(f64::NAN);
    }
    match op {
        BinOp::Add => add(a, b),
        BinOp::Sub => add(a, unary(UnOp::Neg, Ty::F64, b)),
        BinOp::Mul => mul(a, b),
        BinOp::Div => div(a, b),
        BinOp::Rem => rem(a, b),
        _ => Fact::top(Ty::F64),
    }
}

fn add(a: Fact, b: Fact) -> Fact {
    let opposite_inf = (a.hi == f64::INFINITY && b.lo == f64::NEG_INFINITY)
        || (a.lo == f64::NEG_INFINITY && b.hi == f64::INFINITY);
    let lo = a.lo + b.lo;
    let hi = a.hi + b.hi;
    Fact {
        lo: if lo.is_nan() { f64::NEG_INFINITY } else { lo },
        hi: if hi.is_nan() { f64::INFINITY } else { hi },
        integral: a.integral && b.integral,
        nan: a.nan || b.nan || opposite_inf,
        neg_zero: a.neg_zero && b.neg_zero,
    }
}

/// The smallest interval holding the corner values, or `None` when one is NaN.
fn corners(a: Fact, b: Fact, f: fn(f64, f64) -> f64) -> Option<(f64, f64)> {
    let vs = [f(a.lo, b.lo), f(a.lo, b.hi), f(a.hi, b.lo), f(a.hi, b.hi)];
    if vs.iter().any(|v| v.is_nan()) {
        return None;
    }
    let lo = vs.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = vs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some((lo, hi))
}

/// Can the operands' signs differ (counting `-0` as negative)?
fn signs_may_differ(a: Fact, b: Fact) -> bool {
    let neg = |x: Fact| x.lo < 0.0 || x.neg_zero;
    let pos = |x: Fact| x.hi >= 0.0;
    (neg(a) && pos(b)) || (pos(a) && neg(b))
}

fn mul(a: Fact, b: Fact) -> Fact {
    // A zero (or, for fractions, an underflow) with operands of different signs is -0.
    let zero = a.may_be_zero() || b.may_be_zero() || !(a.integral && b.integral);
    let neg_zero = zero && signs_may_differ(a, b);
    let integral = a.integral && b.integral;
    let infinite = |x: Fact| x.lo == f64::NEG_INFINITY || x.hi == f64::INFINITY;
    let zero_inf = (a.may_be_zero() && infinite(b)) || (b.may_be_zero() && infinite(a));
    match corners(a, b, |x, y| x * y) {
        Some((lo, hi)) => Fact {
            lo,
            hi,
            integral,
            nan: a.nan || b.nan || zero_inf,
            neg_zero,
        },
        // 0 * ∞
        None => Fact {
            nan: true,
            integral,
            neg_zero,
            ..Fact::top(Ty::F64)
        },
    }
}

fn div(a: Fact, b: Fact) -> Fact {
    let neg_zero = signs_may_differ(a, b);
    let safe = !b.may_be_zero() && b.lo.is_finite() && b.hi.is_finite();
    match corners(a, b, |x, y| x / y).filter(|_| safe) {
        Some((lo, hi)) => Fact {
            lo,
            hi,
            integral: false,
            nan: a.nan || b.nan,
            neg_zero,
        },
        None => Fact {
            neg_zero,
            ..Fact::top(Ty::F64)
        },
    }
}

/// JS `%` (C `fmod`): the sign of the dividend, smaller in magnitude than the divisor.
fn rem(a: Fact, b: Fact) -> Fact {
    let infinite_a = a.lo == f64::NEG_INFINITY || a.hi == f64::INFINITY;
    let integral = a.integral && b.integral;
    let mut m = b.magnitude();
    if integral && m.is_finite() {
        m -= 1.0;
    }
    let bound = m.min(a.magnitude());
    Fact {
        lo: if a.lo >= 0.0 { 0.0 } else { -bound },
        hi: if a.hi <= 0.0 { 0.0 } else { bound },
        integral,
        nan: a.nan || b.nan || infinite_a || b.may_be_zero(),
        neg_zero: a.lo < 0.0 || a.neg_zero,
    }
}

/// Facts about `a as to` (Rust `as`: floats truncate and saturate, NaN becomes 0) for an
/// operand of type `from`.
pub(super) fn cast(from: Ty, to: Ty, a: Fact) -> Fact {
    match (from.is_float(), to) {
        (false, Ty::F64) if from.is_int() || from == Ty::Bool => Fact {
            neg_zero: false,
            nan: false,
            integral: true,
            ..a
        },
        (true, Ty::F64) if from == Ty::F64 => a,
        (true, t) if t.is_int() => {
            if a.lo > a.hi {
                return Fact::int(0, 0);
            }
            let r = Interval::full(t);
            let sat = |x: f64| x.trunc().clamp(r.lo as f64, r.hi as f64);
            let f = Fact::int(sat(a.lo) as i128, sat(a.hi) as i128);
            if a.nan {
                f.join(Fact::int(0, 0))
            } else {
                f
            }
        }
        (false, t) if t.is_int() && (from.is_int() || from == Ty::Bool) => match a.interval() {
            Some(i) => from_interval(i, t),
            None => Fact::top(t),
        },
        _ => Fact::top(to),
    }
}

/// `trunc`, `floor`, `ceil` and `round` of a double: whole (or NaN, ±∞), between the
/// operand's bounds rounded outwards; `-0` from negative operands.
pub(super) fn rounded(a: Fact) -> Fact {
    Fact {
        lo: a.lo.floor(),
        hi: a.hi.ceil(),
        integral: true,
        nan: a.nan,
        neg_zero: a.neg_zero || a.lo < 0.0,
    }
}

/// `|a|` of a double.
pub(super) fn abs(a: Fact) -> Fact {
    let lo = if a.may_be_zero() {
        0.0
    } else {
        a.lo.abs().min(a.hi.abs())
    };
    Fact {
        lo,
        hi: a.magnitude(),
        integral: a.integral,
        nan: a.nan,
        neg_zero: false,
    }
}

/// The outcome of the comparison `a op b` when the facts decide it.
pub(super) fn decide(op: BinOp, a: Fact, b: Fact) -> Option<bool> {
    let nan = a.nan || b.nan;
    let disjoint = a.hi < b.lo || b.hi < a.lo;
    // NaN compares false (`!=`: true), so a comparison false for every number stays false.
    match op {
        BinOp::Lt if a.hi < b.lo && !nan => Some(true),
        BinOp::Lt if a.lo >= b.hi => Some(false),
        BinOp::Le if a.hi <= b.lo && !nan => Some(true),
        BinOp::Le if a.lo > b.hi => Some(false),
        BinOp::Gt => decide(BinOp::Lt, b, a),
        BinOp::Ge => decide(BinOp::Le, b, a),
        BinOp::Eq if disjoint => Some(false),
        BinOp::Ne if disjoint => Some(true),
        _ => None,
    }
}

#[cfg(test)]
#[path = "fact_tests.rs"]
mod tests;
