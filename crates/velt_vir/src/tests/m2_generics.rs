//! `tests/golden/m2/generics.vlt` hand-lowered to HIR: generic structs/functions/classes
//! (monomorphized per type args), partial moves out of an owned param, interfaces with default
//! methods used as generic bounds (`Callee::ParamMethod`) and as values (`Dyn`).

use velt_sema::hir::{
    AdtKind, BinOp as B, Callee, Def, ImplDef, InterfaceDef, InterfaceMethodDef, Intrinsic,
    PassMode, PatKind, Program, TyKind, UseMode as U,
};

use super::builder::*;
use super::builder_m2::*;
use super::{m2_golden, run};

pub(super) fn generics() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let (p0, p1) = (pb.param(0), pb.param(1));
    let (a0, a1) = (pb.arr(p0), pb.arr(p1));
    // struct Pair<A, B>, swap<A, B>
    let pair_d = pb.declare();
    let mut pd = adt(
        "Pair",
        AdtKind::Struct,
        vec![("first", p0, None), ("second", p1, None)],
    );
    pd.generics = 2;
    pb.set_def(pair_d, Def::Adt(pd));
    let pair_ab = pb.adt_ty(pair_d, vec![p0, p1]);
    let pair_ba = pb.adt_ty(pair_d, vec![p1, p0]);
    let swap = {
        let mut f = FB::new("swap", pair_ba);
        f.generics = 2;
        let p = f.param("p", pair_ab, PassMode::Owned);
        let lit = adt_lit(
            pair_d,
            vec![
                field(f.bw(p), 1, U::Move, p1),
                field(f.bw(p), 0, U::Move, p0),
            ],
            pair_ba,
        );
        pb.add_fn(f.build(vec![ret(Some(lit))]))
    };
    // mapAll<T, U>
    let map_all = {
        let fty = pb.fn_ty(vec![p0], p1);
        let mut f = FB::new("mapAll", a1);
        f.generics = 2;
        let xs = f.param("xs", a0, PassMode::Borrow);
        let cb = f.param("f", fty, PassMode::Borrow);
        let out = f.local("out", a1);
        let x = f.local("x", p0);
        let push = intr(
            Intrinsic::ArrayPush,
            vec![f.bm(out), call_ptr(f.bw(cb), vec![f.bw(x)], p1)],
            t.unit,
        );
        let body = vec![
            let_(out, array(vec![], a1)),
            for_of(pbind(x, U::Borrow, p0), f.bw(xs), vec![se(push)]),
            ret(Some(f.mv(out))),
        ];
        pb.add_fn(f.build(body))
    };
    // class Stack<T>
    let stack_d = pb.declare();
    let mut sd = adt(
        "Stack",
        AdtKind::Class,
        vec![("items", a0, Some(array(vec![], a0)))],
    );
    sd.generics = 1;
    pb.set_def(stack_d, Def::Adt(sd));
    let stack_t = pb.adt_ty(stack_d, vec![p0]);
    let o0 = pb.opt(p0);
    let items = |f: &FB, this, mode| field(f.get(this, mode), 0, mode, a0);
    let (push, pop, size) = {
        let mut f = FB::method("Stack.push", stack_t, t.unit);
        f.generics = 1;
        let this = f.param("this", stack_t, PassMode::BorrowMut);
        let x = f.param("x", p0, PassMode::Owned);
        let b = vec![se(intr(
            Intrinsic::ArrayPush,
            vec![items(&f, this, U::BorrowMut), f.mv(x)],
            t.unit,
        ))];
        let push = pb.add_fn(f.build(b));
        let mut f = FB::method("Stack.pop", stack_t, o0);
        f.generics = 1;
        let this = f.param("this", stack_t, PassMode::BorrowMut);
        let b = vec![ret(Some(intr(
            Intrinsic::ArrayPop,
            vec![items(&f, this, U::BorrowMut)],
            o0,
        )))];
        let pop = pb.add_fn(f.build(b));
        let mut f = FB::method("Stack.size", stack_t, t.usize);
        f.generics = 1;
        let this = f.param("this", stack_t, PassMode::Borrow);
        let b = vec![ret(Some(intr(
            Intrinsic::ArrayLen,
            vec![items(&f, this, U::Borrow)],
            t.usize,
        )))];
        (push, pop, pb.add_fn(f.build(b)))
    };
    // interface Shape { area(): f64; describe(): string { … } }
    let shape_i = pb.declare();
    let area_call = |recv| {
        callee(
            Callee::ParamMethod {
                iface: shape_i,
                iface_args: vec![],
                slot: 0,
                method_type_args: vec![],
            },
            vec![recv],
            t.f64,
        )
    };
    let describe_default = {
        let mut f = FB::method("Shape.describe", p0, t.str);
        f.generics = 1;
        let this = f.param("this", p0, PassMode::Borrow);
        let b = vec![ret(Some(concat(
            s("shape with area ", t),
            to_s(area_call(f.bw(this)), t),
            t,
        )))];
        pb.add_fn(f.build(b))
    };
    pb.set_def(
        shape_i,
        Def::Interface(InterfaceDef {
            name: "Shape".into(),
            generics: 0,
            fields: vec![],
            methods: vec![
                InterfaceMethodDef {
                    name: "area".into(),
                    default: None,
                },
                InterfaceMethodDef {
                    name: "describe".into(),
                    default: Some(describe_default),
                },
            ],
            span: SP,
        }),
    );
    let square_d = pb.add_def(Def::Adt(adt(
        "Square",
        AdtKind::Struct,
        vec![("side", t.f64, None)],
    )));
    let square = pb.adt_ty(square_d, vec![]);
    let circle_d = pb.add_def(Def::Adt(adt(
        "Circle",
        AdtKind::Struct,
        vec![("r", t.f64, None)],
    )));
    let circle = pb.adt_ty(circle_d, vec![]);
    let side = |f: &FB, this| field(f.bw(this), 0, U::Copy, t.f64);
    let sq_area = {
        let mut f = FB::method("Square.area", square, t.f64);
        let this = f.param("this", square, PassMode::Borrow);
        let b = vec![ret(Some(bin(B::Mul, side(&f, this), side(&f, this))))];
        pb.add_fn(f.build(b))
    };
    let (ci_area, ci_describe) = {
        let mut f = FB::method("Circle.area", circle, t.f64);
        let this = f.param("this", circle, PassMode::Borrow);
        let b = vec![ret(Some(bin(
            B::Mul,
            bin(B::Mul, flt(3.0, t.f64), side(&f, this)),
            side(&f, this),
        )))];
        let area = pb.add_fn(f.build(b));
        let mut f = FB::method("Circle.describe", circle, t.str);
        let this = f.param("this", circle, PassMode::Borrow);
        let b = vec![ret(Some(concat(
            s("circle r=", t),
            to_s(side(&f, this), t),
            t,
        )))];
        (area, pb.add_fn(f.build(b)))
    };
    for (ty, methods) in [
        (square, vec![sq_area, describe_default]),
        (circle, vec![ci_area, ci_describe]),
    ] {
        pb.add_impl(ImplDef {
            ty,
            generics: 0,
            iface: shape_i,
            iface_args: vec![],
            methods,
        });
    }
    let biggest = {
        let mut f = FB::new("biggest", t.f64);
        f.generics = 1;
        let xs = f.param("xs", a0, PassMode::Borrow);
        let best = f.local("best", t.f64);
        let x = f.local("x", p0);
        let body = vec![
            let_(best, flt(0.0, t.f64)),
            for_of(
                pbind(x, U::Borrow, p0),
                f.bw(xs),
                vec![if_(
                    cmp(B::Gt, area_call(f.bw(x)), f.cp(best), t),
                    vec![se(assign(f.bm(best), area_call(f.bw(x)), t))],
                    None,
                )],
            ),
            ret(Some(f.cp(best))),
        ];
        pb.add_fn(f.build(body))
    };
    // main
    let pair_is = pb.adt_ty(pair_d, vec![t.i64, t.str]);
    let pair_si = pb.adt_ty(pair_d, vec![t.str, t.i64]);
    let (ia, sa) = (pb.arr(t.i64), pb.arr(t.str));
    let i2i = pb.fn_ty(vec![t.i64], t.i64);
    let i2s = pb.fn_ty(vec![t.i64], t.str);
    let stack_s = pb.adt_ty(stack_d, vec![t.str]);
    let os = pb.opt(t.str);
    let sq_arr = pb.arr(square);
    let dyn_shape = pb.ty(TyKind::Dyn(shape_i, vec![]));
    let dyn_arr = pb.arr(dyn_shape);
    let dbl = closure_def(
        &mut pb,
        "main::{closure#0}",
        t.i64,
        &[],
        &[("x", t.i64)],
        |f, _, ps| vec![ret(Some(bin(B::Mul, f.cp(ps[0]), int(2, t.i64))))],
    );
    let hash = closure_def(
        &mut pb,
        "main::{closure#1}",
        t.str,
        &[],
        &[("x", t.i64)],
        |f, _, ps| vec![ret(Some(concat(s("#", t), to_s(f.cp(ps[0]), t), t)))],
    );
    let mut f = FB::new("main", t.unit);
    let p = f.local("p", pair_is);
    let sw = f.local("s", pair_si);
    let doubled = f.local("doubled", ia);
    let strs = f.local("strs", sa);
    let st = f.local("st", stack_s);
    let shapes = f.local("shapes", dyn_arr);
    let sh = f.local("sh", dyn_shape);
    let vs: Vec<_> = (0..3).map(|i| f.local(&format!("v{i}"), t.str)).collect();
    let pop_or = |f: &FB, v| {
        match_(
            call_g(pop, vec![t.str], vec![f.bm(st)], os),
            vec![
                (
                    pat(PatKind::Some(Box::new(pbind(v, U::Move, t.str))), os),
                    None,
                    f.mv(v),
                ),
                (pat(PatKind::None, os), None, s("none", t)),
            ],
            t.str,
        )
    };
    let idx = |f: &FB, l, i, ty| index(f.bw(l), int(i, t.usize), U::Copy, ty);
    let sqr = |v| adt_lit(square_d, vec![flt(v, t.f64)], square);
    let body = vec![
        let_(
            p,
            adt_lit(pair_d, vec![int(1, t.i64), s("one", t)], pair_is),
        ),
        let_(sw, call_g(swap, vec![t.i64, t.str], vec![f.mv(p)], pair_si)),
        se(print(
            vec![
                field(f.bw(sw), 0, U::Borrow, t.str),
                field(f.bw(sw), 1, U::Copy, t.i64),
            ],
            t,
        )),
        let_(
            doubled,
            call_g(
                map_all,
                vec![t.i64, t.i64],
                vec![
                    array((1..=3).map(|v| int(v, t.i64)).collect(), ia),
                    closure(dbl, i2i),
                ],
                ia,
            ),
        ),
        se(print(
            vec![
                intr(Intrinsic::ArrayLen, vec![f.bw(doubled)], t.usize),
                idx(&f, doubled, 0, t.i64),
                idx(&f, doubled, 2, t.i64),
            ],
            t,
        )),
        let_(
            strs,
            call_g(
                map_all,
                vec![t.i64, t.str],
                vec![
                    array(vec![int(1, t.i64), int(2, t.i64)], ia),
                    closure(hash, i2s),
                ],
                sa,
            ),
        ),
        se(print(
            vec![index(f.bw(strs), int(1, t.usize), U::Borrow, t.str)],
            t,
        )),
        let_(
            st,
            ex(
                velt_sema::hir::ExprKind::New {
                    def: stack_d,
                    type_args: vec![t.str],
                    args: vec![],
                },
                stack_s,
            ),
        ),
        se(call_g(push, vec![t.str], vec![f.bm(st), s("a", t)], t.unit)),
        se(call_g(push, vec![t.str], vec![f.bm(st), s("b", t)], t.unit)),
        se(print(
            vec![
                call_g(size, vec![t.str], vec![f.bw(st)], t.usize),
                pop_or(&f, vs[0]),
                pop_or(&f, vs[1]),
                pop_or(&f, vs[2]),
            ],
            t,
        )),
        se(print(
            vec![call_g(
                biggest,
                vec![square],
                vec![array(vec![sqr(2.0), sqr(3.0)], sq_arr)],
                t.f64,
            )],
            t,
        )),
        let_(
            shapes,
            array(
                vec![
                    to_dyn(sqr(1.0), 0, dyn_shape),
                    to_dyn(
                        adt_lit(circle_d, vec![flt(1.0, t.f64)], circle),
                        1,
                        dyn_shape,
                    ),
                ],
                dyn_arr,
            ),
        ),
        for_of(
            pbind(sh, U::Borrow, dyn_shape),
            f.bw(shapes),
            vec![se(print(
                vec![callee(Callee::Dyn { slot: 1 }, vec![f.bw(sh)], t.str)],
                t,
            ))],
        ),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

#[test]
fn golden_generics() {
    let out = run(&generics());
    assert_eq!(out.stdout, m2_golden("generics"));
}
