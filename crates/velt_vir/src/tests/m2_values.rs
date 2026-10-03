//! Smaller M2 value kinds: module-level constants, `shared<T>`, `T | null` on classes (null
//! niche), cloning/printing interface values and tagged enums, structural equality.

use velt_sema::hir::{
    AdtKind, BinOp as B, Def, ExprKind, GlobalDef, ImplDef, InterfaceDef, InterfaceMethodDef,
    Intrinsic as I, PassMode, PatKind, TyKind, UseMode as U,
};

use super::builder::*;
use super::builder_m2::*;
use super::run;

#[test]
fn globals_are_rematerialized_constants() {
    let mut pb = PB::new();
    let t = pb.t;
    let vd = pb.declare();
    let vec2 = pb.adt_ty(vd, vec![]);
    pb.set_def(
        vd,
        Def::Adt(adt(
            "Vec2",
            AdtKind::Struct,
            vec![("x", t.f64, None), ("y", t.f64, None)],
        )),
    );
    let origin = pb.add_def(Def::Global(GlobalDef {
        name: "ORIGIN".into(),
        ty: vec2,
        init: adt_lit(vd, vec![flt(0.5, t.f64), flt(2.0, t.f64)], vec2),
        span: SP,
    }));
    let greeting = pb.add_def(Def::Global(GlobalDef {
        name: "HELLO".into(),
        ty: t.str,
        init: s("hello", t),
        span: SP,
    }));
    let g = |d, ty| ex(velt_sema::hir::ExprKind::Global(d), ty);
    let mut f = FB::new("main", t.unit);
    let h = f.local("h", t.str);
    let body = vec![
        let_(h, g(greeting, t.str)),
        se(print(
            vec![
                field(g(origin, vec2), 1, U::Copy, t.f64),
                f.bw(h),
                g(origin, vec2),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "2 hello Vec2 { x: 0.5, y: 2 }\n");
}

#[test]
fn shared_values_are_refcounted() {
    let mut pb = PB::new();
    let t = pb.t;
    let sa = pb.arr(t.str);
    let sh = pb.ty(TyKind::Shared(sa));
    let mut f = FB::new("main", t.unit);
    let a = f.local("a", sh);
    let b = f.local("b", sh);
    let body = vec![
        let_(
            a,
            intr(
                I::SharedNew,
                vec![array(vec![concat(s("x", t), s("y", t), t)], sa)],
                sh,
            ),
        ),
        let_(b, intr(I::Clone, vec![f.bw(a)], sh)),
        se(print(vec![f.bw(a), f.bw(b)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "[ 'xy' ] [ 'xy' ]\n");
}

#[test]
fn nullable_class_uses_null_niche() {
    let mut pb = PB::new();
    let t = pb.t;
    let nd = pb.declare();
    let node = pb.adt_ty(nd, vec![]);
    let on = pb.opt(node);
    pb.set_def(
        nd,
        Def::Adt(adt(
            "Node",
            AdtKind::Class,
            vec![("v", t.str, None), ("next", on, Some(null(on)))],
        )),
    );
    let mut f = FB::new("main", t.unit);
    let a = f.local("a", node);
    let v = f.local("v", node);
    let body = vec![
        let_(
            a,
            adt_lit(
                nd,
                vec![
                    s("head", t),
                    wrap_some(adt_lit(nd, vec![s("tail", t), null(on)], node), on),
                ],
                node,
            ),
        ),
        se(print(vec![f.bw(a)], t)),
        se(print(
            vec![match_(
                field(f.bw(a), 1, U::Borrow, on),
                vec![
                    (
                        pat(PatKind::Some(Box::new(pbind(v, U::Borrow, node))), on),
                        None,
                        field(f.bw(v), 0, U::Borrow, t.str),
                    ),
                    (pat(PatKind::None, on), None, s("none", t)),
                ],
                t.str,
            )],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let v = super::lower_ok(&pb.finish());
    assert!(v.aggs.iter().any(|a| a.name == "Node object"
        && a.fields.len() == 2
        && a.fields[1].0 == crate::vir::Ty::Ptr));
    let out = super::interp::run(&v);
    assert_eq!(out.live_allocs, 0);
    assert_eq!(
        out.stdout,
        "Node { v: 'head', next: Node { v: 'tail', next: null } }\ntail\n"
    );
}

#[test]
fn dyn_values_clone_print_and_enum_eq() {
    let mut pb = PB::new();
    let t = pb.t;
    let named = pb.declare();
    let nd = pb.declare();
    let tag = pb.adt_ty(nd, vec![]);
    pb.set_def(
        nd,
        Def::Adt(adt("Tag", AdtKind::Struct, vec![("name", t.str, None)])),
    );
    let get = {
        let mut f = FB::method("Tag.get", tag, t.str);
        let this = f.param("this", tag, PassMode::Borrow);
        let body = vec![ret(Some(field(f.bw(this), 0, U::Borrow, t.str)))];
        pb.add_fn(f.build(body))
    };
    pb.set_def(
        named,
        Def::Interface(InterfaceDef {
            name: "Named".into(),
            generics: 0,
            fields: vec![],
            methods: vec![InterfaceMethodDef {
                name: "get".into(),
                default: None,
                promise: false,
                throws: None,
            }],
            span: SP,
        }),
    );
    pb.add_impl(ImplDef {
        ty: tag,
        generics: 0,
        iface: named,
        iface_args: vec![],
        methods: vec![get],
    });
    let dn = pb.ty(TyKind::Dyn(named, vec![]));
    let ed = pb.add_def(Def::Enum(enum_def(
        "E",
        vec![("A", vec![t.str], 0), ("B", vec![], 1)],
    )));
    let e = pb.adt_ty(ed, vec![]);
    let mut f = FB::new("main", t.unit);
    let d = f.local("d", dn);
    let d2 = f.local("d2", dn);
    let x = f.local("x", e);
    let body = vec![
        let_(
            d,
            to_dyn(
                adt_lit(nd, vec![concat(s("n", t), s("1", t), t)], tag),
                0,
                dn,
            ),
        ),
        let_(d2, intr(I::Clone, vec![f.bw(d)], dn)),
        se(print(
            vec![
                f.bw(d2),
                callee(
                    velt_sema::hir::Callee::Dyn { slot: 0 },
                    vec![f.bw(d)],
                    t.str,
                ),
            ],
            t,
        )),
        let_(x, variant(ed, 0, vec![concat(s("q", t), s("", t), t)], e)),
        se(print(
            vec![
                f.bw(x),
                cmp(B::Eq, f.bw(x), variant(ed, 0, vec![s("q", t)], e), t),
                cmp(B::NotEq, f.bw(x), variant(ed, 1, vec![], e), t),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "Tag { name: 'n1' } n1\nE.A('q') true true\n");
}

#[test]
fn void_fields_are_zero_sized() {
    // `struct R<T> { code: i32; message: string; value: T }` used as `R<void>` (`IoResult<void>`).
    let mut pb = PB::new();
    let t = pb.t;
    let p0 = pb.param(0);
    let mut r = adt(
        "R",
        AdtKind::Struct,
        vec![
            ("code", t.i32, None),
            ("message", t.str, None),
            ("value", p0, None),
        ],
    );
    r.generics = 1;
    let rd = pb.add_def(Def::Adt(r));
    let rv = pb.adt_ty(rd, vec![t.unit]);
    let unit = || ex(ExprKind::Lit(velt_sema::hir::Lit::Unit), t.unit);
    let lit = |msg: &str| {
        ex(
            ExprKind::AdtLit {
                def: rd,
                type_args: vec![t.unit],
                fields: vec![int(0, t.i32), s(msg, t), unit()],
            },
            rv,
        )
    };
    let mut f = FB::new("main", t.unit);
    let a = f.local("a", rv);
    let b = f.local("b", rv);
    let v = f.local("v", t.unit);
    let body = vec![
        let_(a, lit("ok")),
        let_(b, intr(I::Clone, vec![f.bw(a)], rv)),
        let_(v, field(f.bw(a), 2, U::Copy, t.unit)),
        se(assign(field(f.bm(b), 2, U::Copy, t.unit), unit(), t)),
        se(print(
            vec![
                f.bw(a),
                cmp(B::Eq, f.bw(a), f.bw(b), t),
                field(f.bw(b), 2, U::Copy, t.unit),
                f.cp(v),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let prog = pb.finish();
    let vir = super::lower_ok(&prog);
    let layout = vir.aggs.iter().find(|a| a.name == "R").expect("R layout");
    assert_eq!((layout.size, layout.fields.len()), (32, 2));
    let out = run(&prog);
    assert_eq!(
        out.stdout,
        "R { code: 0, message: 'ok', value: undefined } true undefined undefined\n"
    );
}
