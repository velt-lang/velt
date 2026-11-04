//! Comparisons of a converted integer with a whole double below 2^53 (`int_compare`): made in
//! integers, with the double's results for every integer, including those past 2^53 (where the
//! conversion rounds), the largest `u64`s, and a bound that may be negative.

use super::*;

/// `f(x, y)`: when `-2^52 < y < 2^52`, `(x as f64) op (y as f64)` (or with the operands swapped)
/// gives 1 or 2; otherwise 0. `x` is a `u64` or an `i64`.
fn compare_program(x_ty: Ty, op: BinOp, swapped: bool) -> Program {
    let two_52 = 1i128 << 52;
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[x_ty, Ty::I64], Ty::F64);
    let (x, y) = (fb.param(0), fb.param(1));
    let (c0, c1, xf, yf, c) = (
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::Bool),
    );
    let (entry, mid, test, yes, no, out) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.assign(
        entry,
        c0,
        bin(BinOp::Gt, copy_local(y), int(-two_52, Ty::I64)),
    );
    fb.branch(entry, c0, mid, out);
    fb.assign(mid, c1, bin(BinOp::Lt, copy_local(y), int(two_52, Ty::I64)));
    fb.branch(mid, c1, test, out);
    fb.assign(test, yf, Rvalue::Cast(copy_local(y), Ty::F64));
    fb.assign(test, xf, Rvalue::Cast(copy_local(x), Ty::F64));
    let (a, b) = match swapped {
        false => (copy_local(xf), copy_local(yf)),
        true => (copy_local(yf), copy_local(xf)),
    };
    fb.assign(test, c, bin(op, a, b));
    fb.branch(test, c, yes, no);
    fb.ret(yes, float(1.0, Ty::F64));
    fb.ret(no, float(2.0, Ty::F64));
    fb.ret(out, float(0.0, Ty::F64));
    pb.add(fb.finish());
    as_intrinsics(pb.finish())
}

/// Whether some comparison of `f` still compares doubles.
fn compares_doubles(f: &Function) -> bool {
    f.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(s, Stmt::Assign(_, Rvalue::Binary(op, a, _))
            if super::super::flow::is_comparison(*op)
                && super::super::flow::operand_ty(f, a) == Ty::F64)
    })
}

const OPS: [BinOp; 6] = [
    BinOp::Lt,
    BinOp::Le,
    BinOp::Gt,
    BinOp::Ge,
    BinOp::Eq,
    BinOp::Ne,
];

fn ys() -> Vec<i64> {
    let two_52 = 1i64 << 52;
    vec![-two_52 + 1, -65, -1, 0, 1, 63, 64, two_52 - 1]
}

#[test]
fn unsigned_integers_compare_with_whole_doubles_in_integers() {
    let two_53 = 1u64 << 53;
    let xs = [
        0,
        1,
        63,
        64,
        65,
        (1 << 52) - 1,
        two_53 - 1,
        two_53,
        two_53 + 1,
        1 << 63,
        u64::MAX,
    ];
    for op in OPS {
        for swapped in [false, true] {
            let p = compare_program(Ty::U64, op, swapped);
            let q = narrowed(&p);
            assert!(
                !compares_doubles(&q.funcs[0]),
                "{op:?} swapped {swapped}: still doubles"
            );
            for &x in &xs {
                for y in ys() {
                    let args = [x, y as u64];
                    let want = call(&p, &args);
                    assert_eq!(
                        call(&q, &args),
                        want,
                        "{op:?} swapped {swapped}: f({x}, {y})"
                    );
                }
            }
        }
    }
}

#[test]
fn signed_integers_compare_with_whole_doubles_in_integers() {
    let two_53 = 1i64 << 53;
    let xs = [
        i64::MIN,
        -two_53 - 1,
        -two_53,
        -two_53 + 1,
        -65,
        -1,
        0,
        1,
        64,
        two_53 - 1,
        two_53,
        two_53 + 1,
        i64::MAX,
    ];
    for op in OPS {
        for swapped in [false, true] {
            let p = compare_program(Ty::I64, op, swapped);
            let q = narrowed(&p);
            assert!(
                !compares_doubles(&q.funcs[0]),
                "{op:?} swapped {swapped}: still doubles"
            );
            for &x in &xs {
                for y in ys() {
                    let args = [x as u64, y as u64];
                    let want = call(&p, &args);
                    assert_eq!(
                        call(&q, &args),
                        want,
                        "{op:?} swapped {swapped}: f({x}, {y})"
                    );
                }
            }
        }
    }
}

#[test]
fn a_double_that_may_pass_2_53_is_compared_as_a_double() {
    // Without the `y < 2^52` test the bound is any `i64`: the comparison stays.
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::U64, Ty::I64], Ty::F64);
    let (x, y) = (fb.param(0), fb.param(1));
    let (xf, yf, c) = (fb.local(Ty::F64), fb.local(Ty::F64), fb.local(Ty::Bool));
    let (entry, yes, no) = (fb.block(), fb.block(), fb.block());
    fb.assign(entry, yf, Rvalue::Cast(copy_local(y), Ty::F64));
    fb.assign(entry, xf, Rvalue::Cast(copy_local(x), Ty::F64));
    fb.assign(entry, c, bin(BinOp::Lt, copy_local(xf), copy_local(yf)));
    fb.branch(entry, c, yes, no);
    fb.ret(yes, float(1.0, Ty::F64));
    fb.ret(no, float(2.0, Ty::F64));
    pb.add(fb.finish());
    let p = as_intrinsics(pb.finish());
    let q = narrowed(&p);
    assert!(compares_doubles(&q.funcs[0]));
}

#[test]
fn narrower_integers_extend_before_the_comparison() {
    // `u32`/`u8` zero-extend and `i32` sign-extends to the 64-bit comparison.
    let cases: [(Ty, &[i64]); 3] = [
        (Ty::U32, &[0, 1, 64, 65535, 4294967295]),
        (Ty::U8, &[0, 1, 63, 64, 255]),
        (Ty::I32, &[-2147483648, -65, -1, 0, 1, 64, 2147483647]),
    ];
    for (ty, xs) in cases {
        for op in OPS {
            for swapped in [false, true] {
                let p = compare_program(ty, op, swapped);
                let q = narrowed(&p);
                assert!(
                    !compares_doubles(&q.funcs[0]),
                    "{ty:?} {op:?}: still doubles"
                );
                for &x in xs {
                    for y in ys() {
                        let args = [x as u64, y as u64];
                        let want = call(&p, &args);
                        assert_eq!(
                            call(&q, &args),
                            want,
                            "{ty:?} {op:?} {swapped}: f({x}, {y})"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_constant_bound_compares_in_integers() {
    // `(x as f64) < 100.5` is not whole; `(x as f64) <= 100` is.
    for (bound, whole) in [(100.0, true), (-3.0, true), (100.5, false)] {
        let mut pb = ProgramBuilder::new();
        let mut fb = FuncBuilder::export("f", &[Ty::U64], Ty::F64);
        let x = fb.param(0);
        let (xf, c) = (fb.local(Ty::F64), fb.local(Ty::Bool));
        let (entry, yes, no) = (fb.block(), fb.block(), fb.block());
        fb.assign(entry, xf, Rvalue::Cast(copy_local(x), Ty::F64));
        fb.assign(
            entry,
            c,
            bin(BinOp::Le, copy_local(xf), float(bound, Ty::F64)),
        );
        fb.branch(entry, c, yes, no);
        fb.ret(yes, float(1.0, Ty::F64));
        fb.ret(no, float(2.0, Ty::F64));
        pb.add(fb.finish());
        let p = as_intrinsics(pb.finish());
        let q = narrowed(&p);
        assert_eq!(compares_doubles(&q.funcs[0]), !whole, "bound {bound}");
        for x in [0u64, 1, 99, 100, 101, 1 << 53, u64::MAX] {
            assert_eq!(call(&q, &[x]), call(&p, &[x]), "bound {bound}: f({x})");
        }
    }
}
