//! `tests/golden/m2/enums_switch.vlt` hand-lowered to HIR (the shape of what sema emits, with
//! tagged enums standing for the unions): payload enums, C-like enums with explicit
//! discriminants, matches with or-patterns / guards / by-reference bindings.

use velt_sema::hir::{BinOp as B, Def, Lit, PassMode, PatKind, Program, UseMode as U};

use super::builder::*;
use super::builder_m2::*;
use super::{m2_golden, run};

pub(super) fn enums() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let shape_d = pb.add_def(Def::Enum(enum_def(
        "Shape",
        vec![
            ("Circle", vec![t.f64], 0),
            ("Rect", vec![t.f64, t.f64], 1),
            ("Empty", vec![], 2),
        ],
    )));
    let shape = pb.adt_ty(shape_d, vec![]);
    let color_d = pb.add_def(Def::Enum(enum_def(
        "Color",
        vec![
            ("Red", vec![], 0),
            ("Green", vec![], 5),
            ("Blue", vec![], 6),
        ],
    )));
    let color = pb.adt_ty(color_d, vec![]);
    let token_d = pb.add_def(Def::Enum(enum_def(
        "Token",
        vec![("Num", vec![t.i64], 0), ("Word", vec![t.str], 1)],
    )));
    let token = pb.adt_ty(token_d, vec![]);
    let f64c = |v| flt(v, t.f64);
    let area = {
        let mut f = FB::new("area", t.f64);
        let sh = f.param("s", shape, PassMode::Copy);
        let (r, w, h) = (
            f.local("r", t.f64),
            f.local("w", t.f64),
            f.local("h", t.f64),
        );
        let m = match_(
            f.cp(sh),
            vec![
                (
                    pvariant(shape_d, 0, vec![pbind(r, U::Copy, t.f64)], shape),
                    None,
                    bin(B::Mul, bin(B::Mul, f64c(3.0), f.cp(r)), f.cp(r)),
                ),
                (
                    pvariant(
                        shape_d,
                        1,
                        vec![pbind(w, U::Copy, t.f64), pbind(h, U::Copy, t.f64)],
                        shape,
                    ),
                    None,
                    bin(B::Mul, f.cp(w), f.cp(h)),
                ),
                (pvariant(shape_d, 2, vec![], shape), None, f64c(0.0)),
            ],
            t.f64,
        );
        pb.add_fn(f.build(vec![ret(Some(m))]))
    };
    let grade = {
        let mut f = FB::new("grade", t.str);
        let n = f.param("n", t.i64, PassMode::Copy);
        let at_least = |f: &mut FB, lo| Some(cmp(B::GtEq, f.cp(n), int(lo, t.i64), t));
        let lit = |v| pat(PatKind::Lit(Lit::Int(v)), t.i64);
        let (ge90, ge80) = (at_least(&mut f, 90), at_least(&mut f, 80));
        let m = match_(
            f.cp(n),
            vec![
                (pwild(t.i64), ge90, s("A", t)),
                (pwild(t.i64), ge80, s("B", t)),
                (
                    pat(PatKind::Or(vec![lit(0), lit(1)]), t.i64),
                    None,
                    s("zero-ish", t),
                ),
                (pwild(t.i64), None, s("C", t)),
            ],
            t.str,
        );
        pb.add_fn(f.build(vec![ret(Some(m))]))
    };
    let show = {
        let mut f = FB::new("show", t.str);
        let tk = f.param("t", token, PassMode::Borrow);
        let (n1, n2, w) = (
            f.local("n", t.i64),
            f.local("n", t.i64),
            f.local("w", t.str),
        );
        let m = match_(
            f.bw(tk),
            vec![
                (
                    pvariant(token_d, 0, vec![pbind(n1, U::Copy, t.i64)], token),
                    Some(cmp(B::Lt, f.cp(n1), int(0, t.i64), t)),
                    concat(s("negative ", t), to_s(f.cp(n1), t), t),
                ),
                (
                    pvariant(token_d, 0, vec![pbind(n2, U::Copy, t.i64)], token),
                    None,
                    concat(s("num ", t), to_s(f.cp(n2), t), t),
                ),
                (
                    pvariant(token_d, 1, vec![pbind(w, U::Borrow, t.str)], token),
                    None,
                    concat(s("word ", t), f.bw(w), t),
                ),
            ],
            t.str,
        );
        pb.add_fn(f.build(vec![ret(Some(m))]))
    };
    let shapes_t = pb.arr(shape);
    let tokens_t = pb.arr(token);
    let mut f = FB::new("main", t.unit);
    let shapes = f.local("shapes", shapes_t);
    let total = f.local("total", t.f64);
    let sh = f.local("s", shape);
    let tokens = f.local("tokens", tokens_t);
    let tk = f.local("t", token);
    let body = vec![
        let_(
            shapes,
            array(
                vec![
                    variant(shape_d, 0, vec![f64c(2.0)], shape),
                    variant(shape_d, 1, vec![f64c(3.0), f64c(4.0)], shape),
                    variant(shape_d, 2, vec![], shape),
                ],
                shapes_t,
            ),
        ),
        let_(total, f64c(0.0)),
        for_of(
            pbind(sh, U::Copy, shape),
            f.bw(shapes),
            vec![se(cassign(
                B::Add,
                f.bm(total),
                call(area, vec![f.cp(sh)], t.f64),
                t,
            ))],
        ),
        se(print(vec![f.cp(total)], t)),
        se(print(
            vec![
                cast(variant(color_d, 1, vec![], color), t.i64),
                cast(variant(color_d, 2, vec![], color), t.i64),
                cmp(
                    B::Eq,
                    variant(color_d, 0, vec![], color),
                    variant(color_d, 0, vec![], color),
                    t,
                ),
            ],
            t,
        )),
        se(print(
            [95, 85, 1, 50]
                .iter()
                .map(|&v| call(grade, vec![int(v, t.i64)], t.str))
                .collect(),
            t,
        )),
        let_(
            tokens,
            array(
                vec![
                    variant(token_d, 0, vec![neg(int(3, t.i64))], token),
                    variant(token_d, 0, vec![int(7, t.i64)], token),
                    variant(token_d, 1, vec![s("hi", t)], token),
                ],
                tokens_t,
            ),
        ),
        for_of(
            pbind(tk, U::Borrow, token),
            f.bw(tokens),
            vec![se(print(vec![call(show, vec![f.bw(tk)], t.str)], t))],
        ),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

#[test]
fn golden_enums_switch() {
    let out = run(&enums());
    assert_eq!(out.stdout, m2_golden("enums_switch"));
}

/// An owned scrutinee: the arm moves the string payload out, the rest is dropped per arm.
#[test]
fn match_moves_payload_out_of_owned_scrutinee() {
    let mut pb = PB::new();
    let t = pb.t;
    let tok_d = pb.add_def(Def::Enum(enum_def(
        "Tok",
        vec![("Pair", vec![t.str, t.str], 0), ("None", vec![], 1)],
    )));
    let tok = pb.adt_ty(tok_d, vec![]);
    let mut f = FB::new("main", t.unit);
    let x = f.local("x", tok);
    let a = f.local("a", t.str);
    let out = f.local("out", t.str);
    let body = vec![
        let_(
            x,
            variant(
                tok_d,
                0,
                vec![
                    concat(s("a", t), s("1", t), t),
                    concat(s("b", t), s("2", t), t),
                ],
                tok,
            ),
        ),
        let_(
            out,
            match_(
                f.mv(x),
                vec![
                    (
                        pvariant(tok_d, 0, vec![pbind(a, U::Move, t.str), pwild(t.str)], tok),
                        None,
                        f.mv(a),
                    ),
                    (pwild(tok), None, s("none", t)),
                ],
                t.str,
            ),
        ),
        se(print(vec![f.bw(out)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "a1\n");
}
