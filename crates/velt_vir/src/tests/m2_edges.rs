//! Less common M2 paths: rest patterns, `catch {}` without a binding, class
//! objects as interface values, generic impls, virtual methods taking owned arguments (thunks),
//! printing tuples, hashing structured values.

use velt_sema::hir::{
    AdtKind, BinOp as B, Callee, Def, ImplDef, InterfaceDef, InterfaceMethodDef, Intrinsic as I,
    PassMode, PatKind, TyKind, UseMode as U,
};

use super::builder::*;
use super::builder_m2::*;
use super::run;

#[test]
fn rest_pattern_and_catch_without_binding() {
    let mut pb = PB::new();
    let t = pb.t;
    let sa = pb.arr(t.str);
    let boom = {
        let mut f = FB::new("boom", t.unit);
        f.throws = Some(t.str);
        pb.add_fn(f.build(vec![se(throw(concat(s("x", t), s("y", t), t), t.never))]))
    };
    let mut f = FB::new("main", t.unit);
    let (a, rest) = (f.local("a", t.str), f.local("rest", sa));
    // Sema gives a binding-less `catch` a local of the caught type.
    let caught = f.local("<caught>", t.str);
    let body = vec![
        let_pat(
            pat(
                PatKind::Array {
                    elems: vec![pbind(a, U::Move, t.str)],
                    rest: Some(rest),
                },
                sa,
            ),
            array(
                vec![
                    concat(s("p", t), s("1", t), t),
                    s("q", t),
                    concat(s("r", t), s("", t), t),
                ],
                sa,
            ),
        ),
        se(print(vec![f.bw(a), f.bw(rest)], t)),
        try_(
            vec![se(call(boom, vec![], t.unit))],
            Some((Some(caught), vec![se(print(vec![s("caught", t)], t))])),
            None,
        ),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "p1 [ 'q', 'r' ]\ncaught\n");
}

#[test]
fn classes_as_interface_values_and_owned_virtual_args() {
    let mut pb = PB::new();
    let t = pb.t;
    let base_d = pb.declare();
    let base = pb.adt_ty(base_d, vec![]);
    let sub_d = pb.declare();
    let sub = pb.adt_ty(sub_d, vec![]);
    let (take_b, take_s) = (pb.declare(), pb.declare());
    let mut bd = adt("Base", AdtKind::Class, vec![("n", t.i64, None)]);
    bd.vtable = vec![take_b];
    pb.set_def(base_d, Def::Adt(bd));
    let mut sd = adt(
        "Sub",
        AdtKind::Class,
        vec![("n", t.i64, None), ("tag", t.str, Some(s("t", t)))],
    );
    sd.vtable = vec![take_s];
    sd.base = Some(base);
    pb.set_def(sub_d, Def::Adt(sd));
    for (def, cls, name, pre) in [
        (take_b, base, "Base.take", "base "),
        (take_s, sub, "Sub.take", "sub "),
    ] {
        let mut f = FB::method(name, cls, t.str);
        let _this = f.param("this", cls, PassMode::Borrow);
        let x = f.param("x", t.str, PassMode::Owned);
        let keep = f.local("keep", t.str);
        let body = vec![
            let_(keep, f.mv(x)),
            ret(Some(concat(s(pre, t), f.bw(keep), t))),
        ];
        pb.define(def, f.build(body));
    }
    let shows = pb.declare();
    let show_d = pb.declare();
    pb.set_def(
        shows,
        Def::Interface(InterfaceDef {
            name: "Shows".into(),
            generics: 0,
            fields: vec![],
            methods: vec![InterfaceMethodDef {
                name: "show".into(),
                default: Some(show_d),
            }],
            span: SP,
        }),
    );
    let p0 = pb.param(0);
    {
        let mut f = FB::method("Shows.show", p0, t.str);
        f.generics = 1;
        let this = f.param("this", p0, PassMode::Borrow);
        let e = callee(
            Callee::Virtual { slot: 0 },
            vec![upcast(f.bw(this), base), s("via dyn", t)],
            t.str,
        );
        pb.define(show_d, f.build(vec![ret(Some(e))]));
    }
    pb.add_impl(ImplDef {
        ty: base,
        generics: 0,
        iface: shows,
        iface_args: vec![],
        methods: vec![show_d],
    });
    let dy = pb.ty(TyKind::Dyn(shows, vec![]));
    let mut f = FB::new("main", t.unit);
    let b = f.local("b", base);
    let d = f.local("d", dy);
    let arg = f.local("arg", t.str);
    let body = vec![
        let_(b, upcast(new_obj(sub_d, vec![], sub), base)),
        let_(arg, concat(s("o", t), s("wned", t), t)),
        se(print(
            vec![callee(
                Callee::Virtual { slot: 0 },
                vec![f.bw(b), f.mv(arg)],
                t.str,
            )],
            t,
        )),
        let_(d, to_dyn(f.mv(b), 0, dy)),
        se(print(
            vec![
                callee(Callee::Dyn { slot: 0 }, vec![f.bw(d)], t.str),
                f.bw(d),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(
        out.stdout,
        "sub owned\nsub via dyn Sub { n: 0, tag: 't' }\n"
    );
}

#[test]
fn generic_impl_and_structured_hash_print() {
    let mut pb = PB::new();
    let t = pb.t;
    let p0 = pb.param(0);
    let box_d = pb.declare();
    let mut bd = adt("Box", AdtKind::Struct, vec![("v", p0, None)]);
    bd.generics = 1;
    pb.set_def(box_d, Def::Adt(bd));
    let box_p = pb.adt_ty(box_d, vec![p0]);
    let sized = pb.declare();
    let size_m = {
        let mut f = FB::method("Box.size", box_p, t.i64);
        f.generics = 1;
        let _this = f.param("this", box_p, PassMode::Borrow);
        pb.add_fn(f.build(vec![ret(Some(int(1, t.i64)))]))
    };
    pb.set_def(
        sized,
        Def::Interface(InterfaceDef {
            name: "Sized".into(),
            generics: 0,
            fields: vec![],
            methods: vec![InterfaceMethodDef {
                name: "size".into(),
                default: None,
            }],
            span: SP,
        }),
    );
    pb.add_impl(ImplDef {
        ty: box_p,
        generics: 1,
        iface: sized,
        iface_args: vec![],
        methods: vec![size_m],
    });
    let count = {
        let mut f = FB::new("count", t.i64);
        f.generics = 1;
        let x = f.param("x", p0, PassMode::Borrow);
        let e = callee(
            Callee::ParamMethod {
                iface: sized,
                iface_args: vec![],
                slot: 0,
                method_type_args: vec![],
            },
            vec![f.bw(x)],
            t.i64,
        );
        pb.add_fn(f.build(vec![ret(Some(e))]))
    };
    let box_s = pb.adt_ty(box_d, vec![t.str]);
    let tup = pb.ty(TyKind::Tuple(vec![t.i64, t.str]));
    let u64t = pb.ty(TyKind::Int(velt_sema::hir::IntTy::U64));
    let mut f = FB::new("main", t.unit);
    let bx = f.local("bx", box_s);
    let h = |e| intr(I::Hash, vec![e], u64t);
    let mk = |v: &str| adt_lit(box_d, vec![concat(s(v, t), s("", t), t)], box_s);
    let body = vec![
        let_(bx, mk("k")),
        se(print(
            vec![call_g(count, vec![box_s], vec![f.bw(bx)], t.i64)],
            t,
        )),
        se(print(
            vec![
                ex(
                    velt_sema::hir::ExprKind::Tuple(vec![int(1, t.i64), s("a", t)]),
                    tup,
                ),
                f.bw(bx),
            ],
            t,
        )),
        se(print(
            vec![
                cmp(B::Eq, h(f.bw(bx)), h(mk("k")), t),
                cmp(B::Eq, h(f.bw(bx)), h(mk("j")), t),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "1\n[ 1, 'a' ] Box { v: 'k' }\ntrue false\n");
}

/// `dispose()` hooks run before the fields drop: on scope exit, for class objects, and for
/// array elements (in element order).
#[test]
fn dispose_hooks() {
    let mut pb = PB::new();
    let t = pb.t;
    let (hd, rd) = (pb.declare(), pb.declare());
    let (ht, rt) = (pb.adt_ty(hd, vec![]), pb.adt_ty(rd, vec![]));
    let dispose = |pb: &mut PB, name: &str, ty, fty, label: &str| {
        let mut f = FB::method(name, ty, t.unit);
        let this = f.param("this", ty, PassMode::BorrowMut);
        let body = vec![se(print(
            vec![s(label, t), field(f.bw(this), 0, U::Borrow, fty)],
            t,
        ))];
        pb.add_fn(f.build(body))
    };
    let hdisp = dispose(&mut pb, "Handle.dispose", ht, t.i64, "dispose handle");
    let rdisp = dispose(&mut pb, "Res.dispose", rt, t.str, "dispose res");
    let mut h = adt("Handle", AdtKind::Struct, vec![("id", t.i64, None)]);
    h.dispose = Some(hdisp);
    pb.set_def(hd, Def::Adt(h));
    let mut r = adt("Res", AdtKind::Class, vec![("name", t.str, None)]);
    r.dispose = Some(rdisp);
    pb.set_def(rd, Def::Adt(r));
    let ha = pb.arr(ht);
    let mut f = FB::new("main", t.unit);
    let a = f.local("a", ht);
    let r = f.local("r", rt);
    let xs = f.local("xs", ha);
    let hl = |v| adt_lit(hd, vec![int(v, t.i64)], ht);
    let body = vec![
        let_(a, hl(1)),
        sblock(vec![let_(r, adt_lit(rd, vec![s("x", t)], rt))]),
        let_(xs, array(vec![hl(2), hl(3)], ha)),
        se(print(vec![s("end", t)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(
        out.stdout,
        "dispose res x\nend\ndispose handle 2\ndispose handle 3\ndispose handle 1\n"
    );
}
