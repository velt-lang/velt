//! Comparisons of a converted integer with a whole double below 2^53 (`int_compare`): made in
//! integers, with the double's results for every integer, including those past 2^53 (where the
//! conversion rounds), the largest `u64`s, and a bound that may be negative. A conversion whose
//! integer changed since, a bound that may be NaN, and a bound that may reach ±2^53 stay double.

use super::*;

const TWO_53: i128 = 1 << 53;

/// `f(x, y)`: when `lo < y < 2^53`, `(x as f64) op (y as f64)` (or with the operands swapped)
/// gives 1 or 2; otherwise 0. `x` is of type `x_ty`.
fn compare_program(x_ty: Ty, op: BinOp, swapped: bool, lo: i128) -> Program {
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
    fb.assign(entry, c0, bin(BinOp::Gt, copy_local(y), int(lo, Ty::I64)));
    fb.branch(entry, c0, mid, out);
    fb.assign(mid, c1, bin(BinOp::Lt, copy_local(y), int(TWO_53, Ty::I64)));
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
pub(super) fn compares_doubles(f: &Function) -> bool {
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

/// Bounds up to the edge of the proof (strictly within ±2^53).
fn ys() -> Vec<i64> {
    let (t53, t52) = (1i64 << 53, 1i64 << 52);
    vec![-t53 + 1, -t52, -65, -1, 0, 1, 63, 64, t52, t53 - 1]
}

/// Runs `p` and the pass's output on every `x` and bound; with `rewritten`, checks that no
/// comparison of doubles is left.
fn agree(p: &Program, xs: &[u64], ys: &[i64], rewritten: bool, what: &str) {
    let q = narrowed(p);
    assert_eq!(!compares_doubles(&q.funcs[0]), rewritten, "{what}");
    for &x in xs {
        for &y in ys {
            let args = [x, y as u64];
            assert_eq!(call(&q, &args), call(p, &args), "{what}: f({x}, {y})");
        }
    }
}

#[test]
fn unsigned_integers_compare_with_whole_doubles_in_integers() {
    let t53 = 1u64 << 53;
    let xs = [
        0,
        1,
        63,
        64,
        65,
        (1 << 52) - 1,
        t53 - 1,
        t53,
        t53 + 1,
        t53 + 2,
        1 << 63,
        u64::MAX,
    ];
    for op in OPS {
        for swapped in [false, true] {
            let what = format!("{op:?} swapped {swapped}");
            // A bound that may be negative: `==` and `!=` stay double, the rest test its sign.
            let p = compare_program(Ty::U64, op, swapped, -TWO_53);
            let eq = matches!(op, BinOp::Eq | BinOp::Ne);
            agree(&p, &xs, &ys(), !eq, &what);
            // A bound never negative: all six in integers.
            let p = compare_program(Ty::U64, op, swapped, -1);
            let ys: Vec<i64> = ys().into_iter().filter(|y| *y >= 0).collect();
            agree(&p, &xs, &ys, true, &format!("{what}, y >= 0"));
        }
    }
}

#[test]
fn signed_integers_compare_with_whole_doubles_in_integers() {
    let t53 = 1i64 << 53;
    let xs = [
        i64::MIN,
        -t53 - 2,
        -t53 - 1,
        -t53,
        -t53 + 1,
        -65,
        -1,
        0,
        1,
        64,
        t53 - 1,
        t53,
        t53 + 1,
        t53 + 2,
        i64::MAX,
    ];
    let xs: Vec<u64> = xs.iter().map(|x| *x as u64).collect();
    for op in OPS {
        for swapped in [false, true] {
            let p = compare_program(Ty::I64, op, swapped, -TWO_53);
            agree(&p, &xs, &ys(), true, &format!("{op:?} swapped {swapped}"));
        }
    }
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
        let xs: Vec<u64> = xs.iter().map(|x| *x as u64).collect();
        for op in [BinOp::Lt, BinOp::Le, BinOp::Gt, BinOp::Ge] {
            for swapped in [false, true] {
                let p = compare_program(ty, op, swapped, -TWO_53);
                agree(&p, &xs, &ys(), true, &format!("{ty:?} {op:?} {swapped}"));
            }
        }
    }
}

#[test]
fn a_bound_that_may_reach_2_53_is_compared_as_a_double() {
    // `-2^53 <= y`: `x = -2^53 - 1` converts to -2^53, so `x == y` holds in doubles.
    let t53 = 1i64 << 53;
    let xs: Vec<u64> = [-t53 - 1, -t53, 0].iter().map(|x| *x as u64).collect();
    let p = compare_program(Ty::I64, BinOp::Eq, false, -TWO_53 - 1);
    agree(&p, &xs, &[-t53, 0], false, "signed, y >= -2^53");
    let p = compare_program(Ty::U64, BinOp::Lt, false, -TWO_53 - 1);
    agree(&p, &[0, 5], &[-t53, 7], false, "unsigned, y >= -2^53");
}

#[test]
fn a_constant_bound_compares_in_integers() {
    // `(x as f64) <= 100.5` is not whole; `2^53 - 1` is the largest bound rewritten; at 2^53,
    // `x = 2^53 + 1` converts to 2^53, so `<=` holds in doubles.
    let t53 = (1u64 << 53) as f64;
    for (bound, rewritten) in [
        (100.0, true),
        (-3.0, true),
        (t53 - 1.0, true),
        (100.5, false),
        (t53, false),
    ] {
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
        assert_eq!(!compares_doubles(&q.funcs[0]), rewritten, "bound {bound}");
        let t = 1u64 << 53;
        for x in [0u64, 1, 99, 100, 101, t - 1, t, t + 1, u64::MAX] {
            assert_eq!(call(&q, &[x]), call(&p, &[x]), "bound {bound}: f({x})");
        }
    }
}

#[test]
fn a_bound_that_may_be_nan_is_compared_as_a_double() {
    // `n` is 5 on one path and NaN on the other: `x < n` is false for NaN.
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::U64, Ty::U64], Ty::F64);
    let (x, which) = (fb.param(0), fb.param(1));
    let (k, n, xf, c) = (
        fb.local(Ty::Bool),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::Bool),
    );
    let (entry, five, nan, test, yes, no) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.assign(entry, k, bin(BinOp::Eq, copy_local(which), int(0, Ty::U64)));
    fb.branch(entry, k, five, nan);
    fb.assign(five, n, Rvalue::Use(float(5.0, Ty::F64)));
    fb.goto(five, test);
    fb.assign(nan, n, Rvalue::Use(float(f64::NAN, Ty::F64)));
    fb.goto(nan, test);
    fb.assign(test, xf, Rvalue::Cast(copy_local(x), Ty::F64));
    fb.assign(test, c, bin(BinOp::Lt, copy_local(xf), copy_local(n)));
    fb.branch(test, c, yes, no);
    fb.ret(yes, float(1.0, Ty::F64));
    fb.ret(no, float(2.0, Ty::F64));
    pb.add(fb.finish());
    let p = as_intrinsics(pb.finish());
    let q = narrowed(&p);
    assert!(compares_doubles(&q.funcs[0]));
    for args in [[0, 0], [0, 1], [7, 0], [7, 1]] {
        assert_eq!(call(&q, &args), call(&p, &args), "f{args:?}");
    }
}

/// What changes `x` between its conversion and the comparison in `stale_program`.
#[derive(Clone, Copy, Debug)]
enum Change {
    Assign,
    Store,
    MemCopy,
    MemCopyDyn,
    MemSet,
}

/// `f(x, w)`: `xf = x as f64`, then `change` sets `x` to `w` (or all ones), then `xf < 5`. The
/// comparison must use the old `x`.
fn stale_program(change: Change) -> Program {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::U64, Ty::U64], Ty::F64);
    let (x0, w) = (fb.param(0), fb.param(1));
    let (x, wl, px, pw, xf, c) = (
        fb.local(Ty::U64),
        fb.local(Ty::U64),
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
        fb.local(Ty::F64),
        fb.local(Ty::Bool),
    );
    let (entry, yes, no) = (fb.block(), fb.block(), fb.block());
    fb.assign(entry, x, Rvalue::Use(copy_local(x0)));
    fb.assign(entry, wl, Rvalue::Use(copy_local(w)));
    fb.assign(entry, px, Rvalue::AddrOf(Place::local(x)));
    fb.assign(entry, pw, Rvalue::AddrOf(Place::local(wl)));
    fb.assign(entry, xf, Rvalue::Cast(copy_local(x), Ty::F64));
    let s = match change {
        Change::Assign => Stmt::Assign(Place::local(x), Rvalue::Use(copy_local(w))),
        Change::Store => Stmt::Assign(deref(px, Ty::U64), Rvalue::Use(copy_local(w))),
        Change::MemCopy => Stmt::MemCopy {
            dst: copy_local(px),
            src: copy_local(pw),
            size: 8,
        },
        Change::MemCopyDyn => Stmt::MemCopyDyn {
            dst: copy_local(px),
            src: copy_local(pw),
            len: int(8, Ty::U64),
            overlapping: false,
        },
        Change::MemSet => Stmt::MemSet {
            dst: copy_local(px),
            byte: int(0xff, Ty::U8),
            len: int(8, Ty::U64),
        },
    };
    fb.push(entry, s);
    fb.assign(
        entry,
        c,
        bin(BinOp::Lt, copy_local(xf), float(5.0, Ty::F64)),
    );
    fb.branch(entry, c, yes, no);
    fb.ret(yes, float(1.0, Ty::F64));
    fb.ret(no, float(2.0, Ty::F64));
    pb.add(fb.finish());
    as_intrinsics(pb.finish())
}

#[test]
fn a_conversion_whose_integer_changed_is_not_compared_as_the_integer() {
    for change in [
        Change::Assign,
        Change::Store,
        Change::MemCopy,
        Change::MemCopyDyn,
        Change::MemSet,
    ] {
        let p = stale_program(change);
        let q = narrowed(&p);
        for args in [[0, u64::MAX], [0, 3], [9, 0], [9, 3], [3, 9]] {
            assert_eq!(call(&q, &args), call(&p, &args), "{change:?}: f{args:?}");
        }
    }
}

/// `f(y)`: when `y < 2^52`, `for (i = 0; y > 0 & i < (y as u64); i += 4) s += (i as f64) + 3`,
/// the exit test already in `int_compare`'s form, as a later run of the pass sees it.
fn signed_bound_loop() -> Program {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::I64], Ty::F64);
    let y = fb.param(0);
    let (g, i, s, yu, cmp, sign, c, t, j, s2, i2) = (
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::F64),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::U64),
    );
    let (entry, pre, head, body, exit, out) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.assign(
        entry,
        g,
        bin(BinOp::Lt, copy_local(y), int(1 << 52, Ty::I64)),
    );
    fb.branch(entry, g, pre, out);
    fb.assign(pre, i, Rvalue::Use(int(0, Ty::U64)));
    fb.assign(pre, s, Rvalue::Use(float(0.0, Ty::F64)));
    fb.goto(pre, head);
    fb.assign(head, yu, Rvalue::Cast(copy_local(y), Ty::U64));
    fb.assign(head, cmp, bin(BinOp::Lt, copy_local(i), copy_local(yu)));
    fb.assign(head, sign, bin(BinOp::Gt, copy_local(y), int(0, Ty::I64)));
    fb.assign(
        head,
        c,
        bin(BinOp::BitAnd, copy_local(sign), copy_local(cmp)),
    );
    fb.branch(head, c, body, exit);
    fb.assign(body, t, Rvalue::Cast(copy_local(i), Ty::F64));
    fb.assign(body, j, bin(BinOp::Add, copy_local(t), float(3.0, Ty::F64)));
    fb.assign(body, s2, bin(BinOp::Add, copy_local(s), copy_local(j)));
    fb.assign(body, s, Rvalue::Use(copy_local(s2)));
    fb.assign(body, i2, bin(BinOp::Add, copy_local(i), int(4, Ty::U64)));
    fb.assign(body, i, Rvalue::Use(copy_local(i2)));
    fb.goto(body, head);
    fb.ret(exit, copy_local(s));
    fb.ret(out, float(-1.0, Ty::F64));
    pb.add(fb.finish());
    as_intrinsics(pb.finish())
}

#[test]
fn the_sign_tested_form_still_bounds_the_counter() {
    // Read as `i < y` on the taken edge, the exit test bounds `i` below 2^52, so `j = i + 3`
    // (and the sum) narrow to integers.
    let p = signed_bound_loop();
    let q = narrowed(&p);
    let f = &q.funcs[0];
    // (The running sum may pass 2^53 and stays a double.)
    let j_double = f.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            s,
            Stmt::Assign(_, Rvalue::Binary(BinOp::Add, _, Operand::Const(_, Ty::F64)))
        )
    });
    assert!(!j_double, "`i + 3` stayed a double sum:\n{f:?}");
    for y in [-5i64, 0, 1, 4, 5, 100, 1 << 20] {
        assert_eq!(call(&q, &[y as u64]), call(&p, &[y as u64]), "f({y})");
    }
}
