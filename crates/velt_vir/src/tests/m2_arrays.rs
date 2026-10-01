//! Arrays and structs: the array parts of `tests/golden/m2/arrays_maps.vlt` (Map is a prelude
//! class), `tests/golden/m2/structs.vlt`, destructuring, bounds checks and array intrinsics.

use velt_sema::hir::{
    AdtKind, BinOp as B, Def, Intrinsic as I, PassMode, PatKind, Program, UseMode as U,
};

use super::builder::*;
use super::builder_m2::*;
use super::run;

pub(super) fn arrays() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let ia = pb.arr(t.i64);
    let iaa = pb.arr(ia);
    let oi = pb.opt(t.i64);
    let sum = {
        let mut f = FB::new("sum", t.i64);
        let xs = f.param("xs", ia, PassMode::Borrow);
        let s_ = f.local("s", t.i64);
        let x = f.local("x", t.i64);
        let body = vec![
            let_(s_, int(0, t.i64)),
            for_of(
                pbind(x, U::Copy, t.i64),
                f.bw(xs),
                vec![se(cassign(B::Add, f.bm(s_), f.cp(x), t))],
            ),
            ret(Some(f.cp(s_))),
        ];
        pb.add_fn(f.build(body))
    };
    let anon_d = pb.declare();
    let anon = pb.adt_ty(anon_d, vec![]);
    pb.set_def(
        anon_d,
        Def::Adt(adt(
            "{x,y}",
            AdtKind::Anon,
            vec![("x", t.f64, None), ("y", t.f64, None)],
        )),
    );
    let mut f = FB::new("main", t.unit);
    let xs = f.local("xs", ia);
    let i = f.local("i", t.i64);
    let last = f.local("last", oi);
    let v = f.local("v", t.i64);
    let (first, second) = (f.local("first", t.i64), f.local("second", t.i64));
    let (x, y) = (f.local("x", t.f64), f.local("y", t.f64));
    let nested = f.local("nested", iaa);
    let ix = |f: &FB, l, n| index(f.bw(l), int(n, t.i64), U::Copy, t.i64);
    let len = |e| intr(I::ArrayLen, vec![e], t.usize);
    let body = vec![
        let_(xs, array(vec![], ia)),
        sblock(vec![
            let_(i, int(0, t.i64)),
            while_(
                None,
                cmp(B::Lt, f.cp(i), int(5, t.i64), t),
                vec![se(intr(
                    I::ArrayPush,
                    vec![f.bm(xs), bin(B::Mul, f.cp(i), f.cp(i))],
                    t.unit,
                ))],
                Some(cassign(B::Add, f.bm(i), int(1, t.i64), t)),
            ),
        ]),
        se(print(
            vec![
                len(f.bw(xs)),
                call(sum, vec![f.bw(xs)], t.i64),
                ix(&f, xs, 4),
            ],
            t,
        )),
        se(assign(
            index(f.bm(xs), int(0, t.i64), U::BorrowMut, t.i64),
            int(100, t.i64),
            t,
        )),
        let_(last, intr(I::ArrayPop, vec![f.bm(xs)], oi)),
        se(print(
            vec![
                ix(&f, xs, 0),
                match_(
                    f.cp(last),
                    vec![
                        (
                            pat(PatKind::Some(Box::new(pbind(v, U::Copy, t.i64))), oi),
                            None,
                            f.cp(v),
                        ),
                        (pat(PatKind::None, oi), None, neg(int(1, t.i64))),
                    ],
                    t.i64,
                ),
                len(f.bw(xs)),
            ],
            t,
        )),
        let_pat(
            pat(
                PatKind::Array {
                    elems: vec![pbind(first, U::Copy, t.i64), pbind(second, U::Copy, t.i64)],
                    rest: None,
                },
                ia,
            ),
            array(vec![int(10, t.i64), int(20, t.i64)], ia),
        ),
        let_pat(
            pat(
                PatKind::Adt {
                    fields: vec![(0, pbind(x, U::Copy, t.f64)), (1, pbind(y, U::Copy, t.f64))],
                },
                anon,
            ),
            adt_lit(anon_d, vec![flt(1.5, t.f64), flt(2.5, t.f64)], anon),
        ),
        se(print(
            vec![
                bin(B::Add, f.cp(first), f.cp(second)),
                bin(B::Add, f.cp(x), f.cp(y)),
            ],
            t,
        )),
        let_(
            nested,
            array(
                vec![
                    array(vec![int(1, t.i64), int(2, t.i64)], ia),
                    array(vec![int(3, t.i64)], ia),
                ],
                iaa,
            ),
        ),
        se(print(
            vec![
                bin(
                    B::Add,
                    index(
                        index(f.bw(nested), int(0, t.i64), U::Borrow, ia),
                        int(1, t.i64),
                        U::Copy,
                        t.i64,
                    ),
                    index(
                        index(f.bw(nested), int(1, t.i64), U::Borrow, ia),
                        int(0, t.i64),
                        U::Copy,
                        t.i64,
                    ),
                ),
                len(f.bw(nested)),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

#[test]
fn golden_arrays_part() {
    let out = run(&arrays());
    assert_eq!(out.stdout, "5 30 16\n100 16 4\n30 4\n5 2\n");
}

pub(super) fn structs() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let pd = pb.declare();
    let point = pb.adt_ty(pd, vec![]);
    let mut def = adt(
        "Point",
        AdtKind::Struct,
        vec![("x", t.f64, None), ("y", t.f64, None)],
    );
    def.is_copy = true;
    pb.set_def(pd, Def::Adt(def));
    let fx = |f: &FB, this, i| field(f.bw(this), i, U::Copy, t.f64);
    let len_m = {
        let mut f = FB::method("Point.len", point, t.f64);
        let this = f.param("this", point, PassMode::Borrow);
        let sq = bin(
            B::Add,
            bin(B::Mul, fx(&f, this, 0), fx(&f, this, 0)),
            bin(B::Mul, fx(&f, this, 1), fx(&f, this, 1)),
        );
        pb.add_fn(f.build(vec![ret(Some(intr(I::Sqrt, vec![sq], t.f64)))]))
    };
    let scale = {
        let mut f = FB::method("Point.scale", point, t.unit);
        let this = f.param("this", point, PassMode::BorrowMut);
        let k = f.param("k", t.f64, PassMode::Copy);
        let body = (0..2)
            .map(|i| {
                se(cassign(
                    B::Mul,
                    field(f.bm(this), i, U::BorrowMut, t.f64),
                    f.cp(k),
                    t,
                ))
            })
            .collect();
        pb.add_fn(f.build(body))
    };
    let add = {
        let mut f = FB::new("add", point);
        let a = f.param("a", point, PassMode::Copy);
        let b = f.param("b", point, PassMode::Copy);
        let lit = adt_lit(
            pd,
            vec![
                bin(B::Add, fx(&f, a, 0), fx(&f, b, 0)),
                bin(B::Add, fx(&f, a, 1), fx(&f, b, 1)),
            ],
            point,
        );
        pb.add_fn(f.build(vec![ret(Some(lit))]))
    };
    let od = pb.declare();
    let anon = pb.adt_ty(od, vec![]);
    pb.set_def(
        od,
        Def::Adt(adt(
            "{name,n}",
            AdtKind::Anon,
            vec![("name", t.str, None), ("n", t.i64, None)],
        )),
    );
    let mut f = FB::new("main", t.unit);
    let p = f.local("p", point);
    let q = f.local("q", point);
    let r = f.local("r", point);
    let o = f.local("o", anon);
    let body = vec![
        let_(
            p,
            adt_lit(pd, vec![flt(3.0, t.f64), flt(4.0, t.f64)], point),
        ),
        let_(q, f.cp(p)),
        se(print(
            vec![call(len_m, vec![f.bw(p)], t.f64), fx(&f, q, 0)],
            t,
        )),
        se(call(scale, vec![f.bm(p), flt(2.0, t.f64)], t.unit)),
        se(print(vec![fx(&f, p, 0), fx(&f, p, 1), fx(&f, q, 0)], t)),
        let_(r, call(add, vec![f.cp(p), f.cp(q)], point)),
        se(print(
            vec![concat(
                concat(to_s(fx(&f, r, 0), t), s(",", t), t),
                to_s(fx(&f, r, 1), t),
                t,
            )],
            t,
        )),
        let_(o, adt_lit(od, vec![s("anon", t), int(1, t.i64)], anon)),
        se(print(
            vec![
                field(f.bw(o), 0, U::Borrow, t.str),
                field(f.bw(o), 1, U::Copy, t.i64),
            ],
            t,
        )),
        se(print(
            vec![
                intr(I::Floor, vec![flt(2.7, t.f64)], t.f64),
                intr(I::Round, vec![flt(-2.5, t.f64)], t.f64),
                intr(I::FAbs, vec![neg(flt(4.5, t.f64))], t.f64),
                intr(I::Ceil, vec![flt(0.5, t.f64)], t.f64),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

#[test]
fn golden_structs_part() {
    let out = run(&structs());
    assert_eq!(out.stdout, "5 3\n6 8 3\n9,12\nanon 1\n2 -2 4.5 1\n");
}

#[test]
fn index_out_of_bounds_panics() {
    let mut pb = PB::new();
    let t = pb.t;
    let sa = pb.arr(t.str);
    let mut f = FB::new("main", t.unit);
    let xs = f.local("xs", sa);
    let body = vec![
        let_(xs, array(vec![s("a", t), s("b", t)], sa)),
        se(print(
            vec![index(f.bw(xs), neg(int(1, t.i64)), U::Borrow, t.str)],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(
        out.stderr,
        "panic: index out of bounds: the len is 2 but the index is -1\n"
    );
    assert_eq!(out.code, 101);
}

/// swap / remove / truncate / with_capacity / clone / eq / hash on arrays of strings.
#[test]
fn array_intrinsics() {
    let mut pb = PB::new();
    let t = pb.t;
    let sa = pb.arr(t.str);
    let u64t = pb.ty(velt_sema::hir::TyKind::Int(velt_sema::hir::IntTy::U64));
    let mut f = FB::new("main", t.unit);
    let xs = f.local("xs", sa);
    let ys = f.local("ys", sa);
    let removed = f.local("removed", t.str);
    let u = |v| int(v, t.usize);
    let body = vec![
        let_(xs, intr(I::ArrayWithCapacity, vec![u(2)], sa)),
        se(intr(
            I::ArrayPush,
            vec![f.bm(xs), concat(s("a", t), s("1", t), t)],
            t.unit,
        )),
        se(intr(
            I::ArrayPush,
            vec![f.bm(xs), concat(s("b", t), s("2", t), t)],
            t.unit,
        )),
        se(intr(
            I::ArrayPush,
            vec![f.bm(xs), concat(s("c", t), s("3", t), t)],
            t.unit,
        )),
        se(intr(I::ArraySwap, vec![f.bm(xs), u(0), u(2)], t.unit)),
        let_(ys, intr(I::Clone, vec![f.bw(xs)], sa)),
        se(print(
            vec![f.bw(xs), intr(I::Eq, vec![f.bw(xs), f.bw(ys)], t.bool)],
            t,
        )),
        let_(removed, intr(I::ArrayRemove, vec![f.bm(xs), u(0)], t.str)),
        se(intr(I::ArrayTruncate, vec![f.bm(ys), u(1)], t.unit)),
        se(print(
            vec![
                f.bw(removed),
                f.bw(xs),
                f.bw(ys),
                intr(I::Eq, vec![f.bw(xs), f.bw(ys)], t.bool),
            ],
            t,
        )),
        se(print(
            vec![cmp(
                B::Eq,
                intr(I::Hash, vec![s("k", t)], u64t),
                intr(I::Hash, vec![concat(s("", t), s("k", t), t)], u64t),
                t,
            )],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(
        out.stdout,
        "[ 'c3', 'b2', 'a1' ] true\nc3 [ 'b2', 'a1' ] [ 'c3' ] false\ntrue\n"
    );
}
