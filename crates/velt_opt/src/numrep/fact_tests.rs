//! Unit tests for the `numrep` facts: every transfer function is checked against the values
//! it describes, computed with Rust's `f64` (which is IEEE, like JS).

use super::*;

fn range(lo: f64, hi: f64, integral: bool) -> Fact {
    Fact {
        lo,
        hi,
        integral,
        nan: false,
        neg_zero: false,
    }
}

/// Doubles in `f`'s interval worth trying: its ends, zeros, ±1, halves and the 2^53 edge.
fn samples(f: Fact) -> Vec<f64> {
    let mut out = vec![];
    let cands = [
        f.lo,
        f.hi,
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        -0.5,
        2.5,
        -7.0,
        7.0,
        TWO_53 - 1.0,
        TWO_53,
        -TWO_53,
        1e300,
        -1e300,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];
    for x in cands {
        let zero_ok = x != 0.0 || !x.is_sign_negative() || f.neg_zero;
        let whole = !f.integral || x.fract() == 0.0 || x.is_infinite();
        if x >= f.lo && x <= f.hi && zero_ok && whole && !x.is_nan() {
            out.push(x);
        }
    }
    if f.nan {
        out.push(f64::NAN);
    }
    out
}

/// `r` describes `v`.
fn holds(r: Fact, v: f64) -> bool {
    if v.is_nan() {
        return r.nan;
    }
    let zero_ok = v != 0.0 || !v.is_sign_negative() || r.neg_zero;
    let whole = !r.integral || v.fract() == 0.0 || v.is_infinite();
    v >= r.lo && v <= r.hi && zero_ok && whole
}

fn check_binary(op: BinOp, a: Fact, b: Fact, f: fn(f64, f64) -> f64) {
    let r = binary(op, Ty::F64, a, b);
    for x in samples(a) {
        for y in samples(b) {
            let v = f(x, y);
            assert!(
                holds(r, v),
                "{op:?} {x} {y} = {v} not in {r:?} ({a:?}, {b:?})"
            );
        }
    }
}

fn facts() -> Vec<Fact> {
    let top = Fact::top(Ty::F64);
    vec![
        top,
        range(0.0, 10.0, true),
        range(-5.0, 5.0, true),
        Fact {
            neg_zero: true,
            ..range(-3.0, 0.0, true)
        },
        range(1.0, 1.0, true),
        range(-0.5, 2.5, false),
        range(TWO_53 - 2.0, TWO_53, true),
        range(1.0, f64::INFINITY, true),
        Fact {
            nan: true,
            ..range(-1.0, 1.0, false)
        },
    ]
}

#[test]
fn arithmetic_contains_every_result() {
    let ops: [(BinOp, fn(f64, f64) -> f64); 5] = [
        (BinOp::Add, |x, y| x + y),
        (BinOp::Sub, |x, y| x - y),
        (BinOp::Mul, |x, y| x * y),
        (BinOp::Div, |x, y| x / y),
        (BinOp::Rem, |x, y| x % y),
    ];
    for (op, f) in ops {
        for a in facts() {
            for b in facts() {
                check_binary(op, a, b, f);
            }
        }
    }
}

#[test]
fn unary_and_rounding_contain_every_result() {
    for a in facts() {
        let neg = unary(UnOp::Neg, Ty::F64, a);
        let round = rounded(a);
        let abs_ = abs(a);
        for x in samples(a) {
            assert!(holds(neg, -x), "-{x} not in {neg:?}");
            for v in [x.trunc(), x.floor(), x.ceil()] {
                assert!(holds(round, v), "{v} not in {round:?}");
            }
            assert!(holds(abs_, x.abs()), "|{x}| not in {abs_:?}");
        }
    }
}

#[test]
fn whole_sums_below_2_53_are_exact_and_bounded() {
    let i = range(0.0, 999.0, true);
    let s = binary(BinOp::Add, Ty::F64, i, Fact::float(1.0));
    assert_eq!((s.lo, s.hi), (1.0, 1000.0));
    assert!(s.exact_int() && !s.neg_zero && s.fits(Ty::I32));
    // 2^53 + 1 rounds: the sum is not provably exact.
    let big = binary(BinOp::Add, Ty::F64, Fact::float(TWO_53), Fact::float(1.0));
    assert!(!big.exact_int());
}

#[test]
fn negative_zero_producers() {
    let z = Fact::float(0.0);
    let small = range(-3.0, 3.0, true);
    assert!(unary(UnOp::Neg, Ty::F64, z).neg_zero);
    assert!(binary(BinOp::Mul, Ty::F64, small, z).neg_zero);
    assert!(binary(BinOp::Rem, Ty::F64, small, Fact::float(2.0)).neg_zero);
    assert!(!binary(BinOp::Rem, Ty::F64, range(0.0, 9.0, true), Fact::float(2.0)).neg_zero);
    assert!(!binary(BinOp::Add, Ty::F64, small, z).neg_zero);
    let nz = Fact::float(-0.0);
    assert!(binary(BinOp::Add, Ty::F64, nz, nz).neg_zero);
    assert!(binary(BinOp::Sub, Ty::F64, nz, z).neg_zero);
    assert!(
        !binary(
            BinOp::Mul,
            Ty::F64,
            range(1.0, 5.0, true),
            range(-5.0, -1.0, true)
        )
        .neg_zero
    );
}

#[test]
fn nan_producers() {
    let top = Fact::top(Ty::F64);
    let int = range(-5.0, 5.0, true);
    assert!(binary(BinOp::Rem, Ty::F64, int, int).nan, "x % 0");
    assert!(!binary(BinOp::Rem, Ty::F64, int, Fact::float(3.0)).nan);
    assert!(
        binary(
            BinOp::Sub,
            Ty::F64,
            range(0.0, f64::INFINITY, true),
            range(0.0, f64::INFINITY, true)
        )
        .nan
    );
    assert!(
        binary(BinOp::Mul, Ty::F64, int, range(1.0, f64::INFINITY, true)).nan,
        "0 * inf"
    );
    assert!(binary(BinOp::Add, Ty::F64, top, int).nan);
}

#[test]
fn casts() {
    let c = cast(Ty::I32, Ty::F64, Fact::top(Ty::I32));
    assert!(c.exact_int() && c.fits(Ty::I32) && !c.neg_zero);
    let u = cast(Ty::U64, Ty::F64, Fact::top(Ty::U64));
    assert!(!u.exact_int());
    // Saturating, NaN to 0.
    let s = cast(Ty::F64, Ty::I32, Fact::top(Ty::F64));
    assert_eq!((s.lo, s.hi), (i32::MIN as f64, i32::MAX as f64));
    let n = cast(Ty::F64, Ty::U8, Fact::float(f64::NAN));
    assert_eq!((n.lo, n.hi), (0.0, 0.0));
    // Integer conversions wrap: a value outside the target is the whole target.
    let w = cast(Ty::I64, Ty::I32, Fact::int(0, 1 << 40));
    assert_eq!(w, Fact::top(Ty::I32));
}

#[test]
fn integer_arithmetic_wraps_to_the_type() {
    let a = Fact::int(0, 100);
    assert_eq!(binary(BinOp::Add, Ty::I64, a, a), Fact::int(0, 200));
    let big = Fact::top(Ty::I32);
    assert_eq!(binary(BinOp::Mul, Ty::I32, big, big), Fact::top(Ty::I32));
    assert_eq!(
        binary(BinOp::Div, Ty::I64, a, Fact::int(0, 3)),
        Fact::top(Ty::I64)
    );
}

#[test]
fn decided_comparisons() {
    let a = range(0.0, 9.0, true);
    let b = range(10.0, 20.0, true);
    assert_eq!(decide(BinOp::Lt, a, b), Some(true));
    assert_eq!(decide(BinOp::Ge, a, b), Some(false));
    assert_eq!(decide(BinOp::Eq, a, b), Some(false));
    assert_eq!(decide(BinOp::Ne, a, b), Some(true));
    let nan = Fact { nan: true, ..a };
    // NaN < 10 is false, so "true" cannot be decided; ">= 10" is false either way.
    assert_eq!(decide(BinOp::Lt, nan, b), None);
    assert_eq!(decide(BinOp::Ge, nan, b), Some(false));
    assert_eq!(decide(BinOp::Lt, a, a), None);
}
