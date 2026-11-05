//! `refine::condition` reading `int_compare`'s sign-tested form back as the comparison it is:
//! `sign = y test 0; cmp = x op (y as u64); c = sign join cmp` is exactly `x op y`. Each form and
//! its negation read back and bound `x` on both edges; anything else built from the same
//! operators is not that comparison and is not read back. Also the slice of the bounds: a bound
//! written through a pointer has no facts.

use super::int_compare::compares_doubles;
use super::*;
use crate::numrep::fact::Fact;
use crate::numrep::flow::Flow;
use crate::numrep::int_compare::sign_test;
use crate::numrep::refine::condition;
use velt_vir::vir::Local;

/// What `form_function` writes around the statements of the form.
#[derive(Clone, Copy, PartialEq)]
enum Extra {
    None,
    /// `y as u64` converts another local.
    CastOfOther,
    /// `x = other` between the comparison and the join.
    XBeforeJoin,
    /// `x = other` after the join.
    XAfterJoin,
    /// `y = other` between the conversion and the sign test.
    YAfterCast,
    /// `y = other` after the join.
    YAfterJoin,
    /// The comparison reads `yu` set from `other`; `yu = y as u64` only comes after it.
    CastAfterCompare,
}

#[derive(Clone, Copy)]
struct Form {
    op: BinOp,
    test: BinOp,
    constant: i128,
    join: BinOp,
    extra: Extra,
    /// Branch on `!c`.
    not: bool,
}

/// The form `int_compare` writes for `x op y`.
fn form(op: BinOp) -> Form {
    let (test, join) = sign_test(op);
    Form {
        op,
        test,
        constant: 0,
        join,
        extra: Extra::None,
        not: false,
    }
}

const ORDERINGS: [BinOp; 4] = [BinOp::Lt, BinOp::Le, BinOp::Gt, BinOp::Ge];

/// `f(x, y, other)`, one block computing `form` and branching on it (`x` and `y` are locals 0
/// and 1).
fn form_function(form: Form) -> Function {
    let mut fb = FuncBuilder::export("f", &[Ty::U64, Ty::I64, Ty::I64], Ty::F64);
    let (x, y, other) = (fb.param(0), fb.param(1), fb.param(2));
    let (yu, cmp, sign, c, n) = (
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
    );
    let (entry, yes, no) = (fb.block(), fb.block(), fb.block());
    let source = match form.extra {
        Extra::CastOfOther | Extra::CastAfterCompare => other,
        _ => y,
    };
    fb.assign(entry, yu, Rvalue::Cast(copy_local(source), Ty::U64));
    if form.extra == Extra::YAfterCast {
        fb.assign(entry, y, Rvalue::Use(copy_local(other)));
    }
    fb.assign(entry, cmp, bin(form.op, copy_local(x), copy_local(yu)));
    if form.extra == Extra::CastAfterCompare {
        fb.assign(entry, yu, Rvalue::Cast(copy_local(y), Ty::U64));
    }
    if form.extra == Extra::XBeforeJoin {
        fb.assign(entry, x, Rvalue::Cast(copy_local(other), Ty::U64));
    }
    let constant = int(form.constant, Ty::I64);
    fb.assign(entry, sign, bin(form.test, copy_local(y), constant));
    fb.assign(entry, c, bin(form.join, copy_local(sign), copy_local(cmp)));
    if form.extra == Extra::XAfterJoin {
        fb.assign(entry, x, Rvalue::Cast(copy_local(other), Ty::U64));
    }
    if form.extra == Extra::YAfterJoin {
        fb.assign(entry, y, Rvalue::Use(copy_local(other)));
    }
    if form.not {
        fb.assign(entry, n, Rvalue::Unary(UnOp::Not, copy_local(c)));
        fb.branch(entry, n, yes, no);
    } else {
        fb.branch(entry, c, yes, no);
    }
    fb.ret(yes, float(1.0, Ty::F64));
    fb.ret(no, float(2.0, Ty::F64));
    fb.finish()
}

/// What `refine::condition` reads `form` as: the operator and whether it is negated. Whatever it
/// reads must compare `x` with `y` itself.
fn read_back(form: Form) -> Option<(BinOp, bool)> {
    let f = form_function(form);
    let c = condition(&f, 0)?;
    let is_local = |op: &Operand, l: u32| matches!(op, Operand::Copy(p) if p.local.0 == l);
    assert!(is_local(&c.lhs, 0), "the left operand is not `x`");
    assert!(is_local(&c.rhs, 1), "the right operand is not `y`");
    Some((c.op, c.negated))
}

#[test]
fn each_form_reads_back_as_its_comparison() {
    for op in ORDERINGS {
        assert_eq!(read_back(form(op)), Some((op, false)), "{op:?}");
        let not = Form {
            not: true,
            ..form(op)
        };
        assert_eq!(read_back(not), Some((op, true)), "!{op:?}");
    }
}

#[test]
fn a_sign_test_against_another_constant_is_not_read_back() {
    // `y > 5 & x < y as u64` is false for `y = 3, x = 0`, where `x < y` holds.
    for op in ORDERINGS {
        for constant in [5, -1] {
            let f = Form {
                constant,
                ..form(op)
            };
            assert_eq!(read_back(f), None, "{op:?} against {constant}");
        }
    }
}

#[test]
fn an_equality_is_not_read_back() {
    // `y < 0 | x == y as u64` holds for any negative `y`, where `x == y` never does.
    for op in [BinOp::Eq, BinOp::Ne] {
        assert_eq!(read_back(form(op)), None, "{op:?}");
    }
}

#[test]
fn a_form_with_the_wrong_test_or_join_is_not_read_back() {
    for op in ORDERINGS {
        let Form { test, join, .. } = form(op);
        for wrong in ORDERINGS.into_iter().filter(|t| *t != test) {
            let f = Form {
                test: wrong,
                ..form(op)
            };
            assert_eq!(read_back(f), None, "{op:?} with test {wrong:?}");
        }
        let wrong = match join {
            BinOp::BitAnd => BinOp::BitOr,
            _ => BinOp::BitAnd,
        };
        let f = Form {
            join: wrong,
            ..form(op)
        };
        assert_eq!(read_back(f), None, "{op:?} with join {wrong:?}");
    }
}

#[test]
fn a_form_whose_operands_change_or_differ_is_not_read_back() {
    for op in ORDERINGS {
        for extra in [
            Extra::CastOfOther,
            Extra::XBeforeJoin,
            Extra::XAfterJoin,
            Extra::YAfterCast,
            Extra::YAfterJoin,
            Extra::CastAfterCompare,
        ] {
            let f = Form { extra, ..form(op) };
            assert_eq!(read_back(f), None, "{op:?}");
        }
    }
}

/// `f(x, y)`: when `lo <= y <= hi`, branches on `form` of `x` and `y`, reading `x` on each edge,
/// and gives 1 on the taken edge and 2 on the other; otherwise 0. Returns the two edges' blocks
/// too.
fn edges_program(form: Form, lo: i128, hi: i128) -> (Program, BlockId, BlockId) {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::U64, Ty::I64], Ty::F64);
    let (x, y) = (fb.param(0), fb.param(1));
    let (g0, g1, yu, cmp, sign, c, n, r) = (
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
        fb.local(Ty::F64),
    );
    let (entry, mid, test, yes, no, out) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.assign(entry, g0, bin(BinOp::Ge, copy_local(y), int(lo, Ty::I64)));
    fb.branch(entry, g0, mid, out);
    fb.assign(mid, g1, bin(BinOp::Le, copy_local(y), int(hi, Ty::I64)));
    fb.branch(mid, g1, test, out);
    fb.assign(test, yu, Rvalue::Cast(copy_local(y), Ty::U64));
    fb.assign(test, cmp, bin(form.op, copy_local(x), copy_local(yu)));
    fb.assign(test, sign, bin(form.test, copy_local(y), int(0, Ty::I64)));
    fb.assign(test, c, bin(form.join, copy_local(sign), copy_local(cmp)));
    if form.not {
        fb.assign(test, n, Rvalue::Unary(UnOp::Not, copy_local(c)));
        fb.branch(test, n, yes, no);
    } else {
        fb.branch(test, c, yes, no);
    }
    for (b, v) in [(yes, 1.0), (no, 2.0)] {
        fb.assign(b, r, Rvalue::Cast(copy_local(x), Ty::F64));
        fb.ret(b, float(v, Ty::F64));
    }
    fb.ret(out, float(0.0, Ty::F64));
    pb.add(fb.finish());
    (as_intrinsics(pb.finish()), yes, no)
}

/// The bounds of `x` the analysis has on the edge into `b`.
fn x_bounds(p: &Program, b: BlockId) -> (f64, f64) {
    let env = Env::of(&p.externs, &p.funcs, &p.aggs, &p.statics);
    let f = &p.funcs[0];
    let flow = Flow::compute(f, &env, &[Local(0), Local(1)], vec![]).expect("small enough");
    let fact = flow
        .fact_before(f, b.0 as usize, 0, Local(0))
        .expect("the edge is reachable");
    (fact.lo, fact.hi)
}

#[test]
fn both_edges_of_the_form_bound_the_integer() {
    // `y` is within [5, 1000]; `x` starts as any `u64`.
    let top = Fact::top(Ty::U64).hi;
    // Per operator: the bounds of `x` when the comparison holds, and when it does not.
    let cases = [
        (BinOp::Lt, (0.0, 999.0), (5.0, top)),
        (BinOp::Le, (0.0, 1000.0), (6.0, top)),
        (BinOp::Gt, (6.0, top), (0.0, 1000.0)),
        (BinOp::Ge, (5.0, top), (0.0, 999.0)),
    ];
    for (op, holds, fails) in cases {
        let (p, yes, no) = edges_program(form(op), 5, 1000);
        assert_eq!(x_bounds(&p, yes), holds, "{op:?}, taken");
        assert_eq!(x_bounds(&p, no), fails, "{op:?}, not taken");
        let not = Form {
            not: true,
            ..form(op)
        };
        let (p, yes, no) = edges_program(not, 5, 1000);
        assert_eq!(x_bounds(&p, yes), fails, "!{op:?}, taken");
        assert_eq!(x_bounds(&p, no), holds, "!{op:?}, not taken");
    }
}

#[test]
fn a_negative_bound_decides_the_form_through_its_sign() {
    // `x < y` and `x <= y` never hold for an unsigned `x` and a negative `y`, `x > y` and
    // `x >= y` always do, even where `y as u64` is close to `x`.
    for op in ORDERINGS {
        let (p, _, _) = edges_program(form(op), -20, -5);
        let q = narrowed(&p);
        let want = match op {
            BinOp::Lt | BinOp::Le => 2.0f64,
            _ => 1.0,
        };
        for x in [0, 1, 5, u64::MAX - 20, u64::MAX - 4, u64::MAX] {
            for y in [-20i64, -6, -5] {
                let args = [x, y as u64];
                assert_eq!(call(&p, &args), Ok(want.to_bits()), "{op:?}: f({x}, {y})");
                assert_eq!(call(&q, &args), Ok(want.to_bits()), "{op:?}: f({x}, {y})");
            }
        }
    }
}

#[test]
fn an_address_taken_bound_has_no_facts() {
    // `y` is 5 by direct assignment, but a store through a pointer makes it 0.5: `1 < y` is false
    // in doubles, and would be true compared with the 5 that facts about `y` would claim.
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::U64], Ty::F64);
    let x = fb.param(0);
    let (y, py, xf, c) = (
        fb.local(Ty::F64),
        fb.local(Ty::Ptr),
        fb.local(Ty::F64),
        fb.local(Ty::Bool),
    );
    let (entry, yes, no) = (fb.block(), fb.block(), fb.block());
    fb.assign(entry, y, Rvalue::Use(float(5.0, Ty::F64)));
    fb.assign(entry, py, Rvalue::AddrOf(Place::local(y)));
    fb.push(
        entry,
        Stmt::Assign(deref(py, Ty::F64), Rvalue::Use(float(0.5, Ty::F64))),
    );
    fb.assign(entry, xf, Rvalue::Cast(copy_local(x), Ty::F64));
    fb.assign(entry, c, bin(BinOp::Lt, copy_local(xf), copy_local(y)));
    fb.branch(entry, c, yes, no);
    fb.ret(yes, float(1.0, Ty::F64));
    fb.ret(no, float(2.0, Ty::F64));
    pb.add(fb.finish());
    let p = as_intrinsics(pb.finish());
    let q = narrowed(&p);
    assert!(compares_doubles(&q.funcs[0]), "the bound was given facts");
    for x in [0, 1, 4, 5, 6] {
        assert_eq!(call(&q, &[x]), call(&p, &[x]), "f({x})");
    }
}
