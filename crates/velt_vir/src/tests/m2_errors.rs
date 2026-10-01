//! The throwing part of `tests/golden/m2/errors.vlt` hand-lowered to HIR: throwing functions
//! (Result via out-pointer), propagation, try/catch/finally, options.
//! (`??` is sema sugar; written here as the match it lowers to.)

use velt_sema::hir::{
    AdtKind, BinOp as B, Def, Intrinsic, PassMode, PatKind, Program, StmtKind, UseMode as U,
};

use super::builder::*;
use super::builder_m2::*;
use super::run;

pub(super) fn errors() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let pe_d = pb.declare();
    let pe = pb.adt_ty(pe_d, vec![]);
    let pe_ctor = pb.declare();
    let mut pd = adt("ParseError", AdtKind::Class, vec![("message", t.str, None)]);
    pd.ctor = Some(pe_ctor);
    pb.set_def(pe_d, Def::Adt(pd));
    {
        let mut f = FB::method("ParseError.constructor", pe, t.unit);
        let this = f.param("this", pe, PassMode::BorrowMut);
        let m = f.param("message", t.str, PassMode::Owned);
        let body = vec![se(assign(
            field(f.bm(this), 0, U::BorrowMut, t.str),
            f.mv(m),
            t,
        ))];
        pb.define(pe_ctor, f.build(body));
    }
    let parse_digit = {
        let mut f = FB::new("parseDigit", t.i64);
        f.throws = Some(pe);
        let sp = f.param("s", t.str, PassMode::Borrow);
        let body = vec![
            if_(
                cmp(B::Eq, f.bw(sp), s("0", t), t),
                vec![ret(Some(int(0, t.i64)))],
                None,
            ),
            if_(
                cmp(B::Eq, f.bw(sp), s("1", t), t),
                vec![ret(Some(int(1, t.i64)))],
                None,
            ),
            se(throw(
                new_obj(pe_d, vec![concat(s("bad digit: ", t), f.bw(sp), t)], pe),
                t.never,
            )),
        ];
        pb.add_fn(f.build(body))
    };
    let sum_digits = {
        let mut f = FB::new("sumDigits", t.i64);
        f.throws = Some(pe);
        let a = f.param("a", t.str, PassMode::Borrow);
        let b = f.param("b", t.str, PassMode::Borrow);
        let e = bin(
            B::Add,
            call(parse_digit, vec![f.bw(a)], t.i64),
            call(parse_digit, vec![f.bw(b)], t.i64),
        );
        pb.add_fn(f.build(vec![ret(Some(e))]))
    };
    let names_t = pb.arr(t.str);
    let oi = pb.opt(t.i64);
    let find = {
        let mut f = FB::new("find", oi);
        let xs = f.param("xs", names_t, PassMode::Borrow);
        let want = f.param("want", t.str, PassMode::Borrow);
        let i = f.local("i", t.usize);
        let len = intr(Intrinsic::ArrayLen, vec![f.bw(xs)], t.usize);
        let body = vec![
            sblock(vec![
                let_(i, int(0, t.usize)),
                while_(
                    None,
                    cmp(B::Lt, f.cp(i), len, t),
                    vec![if_(
                        cmp(
                            B::Eq,
                            index(f.bw(xs), f.cp(i), U::Borrow, t.str),
                            f.bw(want),
                            t,
                        ),
                        vec![ret(Some(wrap_some(cast(f.cp(i), t.i64), oi)))],
                        None,
                    )],
                    Some(cassign(B::Add, f.bm(i), int(1, t.usize), t)),
                ),
            ]),
            ret(Some(null(oi))),
        ];
        pb.add_fn(f.build(body))
    };
    let mut f = FB::new("main", t.unit);
    f.throws = Some(pe);
    let e = f.local("e", pe);
    let v3 = f.local("v", t.i64);
    let names = f.local("names", names_t);
    let idx = f.local("idx", oi);
    let sum = |a, b| call(sum_digits, vec![s(a, t), s(b, t)], t.i64);
    let body = vec![
        se(print(vec![sum("1", "1")], t)),
        try_(
            vec![
                se(print(vec![sum("1", "x")], t)),
                se(print(vec![s("unreachable", t)], t)),
            ],
            Some((
                Some(e),
                vec![se(print(
                    vec![s("caught:", t), field(f.bw(e), 0, U::Borrow, t.str)],
                    t,
                ))],
            )),
            Some(vec![se(print(vec![s("finally", t)], t))]),
        ),
        let_(names, array(vec![s("ann", t), s("bob", t)], names_t)),
        let_(idx, call(find, vec![f.bw(names), s("bob", t)], oi)),
        se(match_(
            f.cp(idx),
            vec![
                (
                    pat(PatKind::Some(Box::new(pwild(t.i64))), oi),
                    None,
                    bexpr(
                        vec![se(print(
                            vec![s("found at", t), unwrap_some(f.cp(idx), U::Copy, t.i64)],
                            t,
                        ))],
                        ex_unit(t),
                    ),
                ),
                (pat(PatKind::None, oi), None, ex_unit(t)),
            ],
            t.unit,
        )),
        se(print(
            vec![match_(
                call(find, vec![f.bw(names), s("zed", t)], oi),
                vec![
                    (
                        pat(PatKind::Some(Box::new(pbind(v3, U::Copy, t.i64))), oi),
                        None,
                        f.cp(v3),
                    ),
                    (pat(PatKind::None, oi), None, neg(int(1, t.i64))),
                ],
                t.i64,
            )],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

fn ex_unit(t: T) -> velt_sema::hir::Expr {
    ex(
        velt_sema::hir::ExprKind::Lit(velt_sema::hir::Lit::Unit),
        t.unit,
    )
}

#[test]
fn hand_lowered_errors() {
    let out = run(&errors());
    assert_eq!(
        out.stdout,
        "2
caught: bad digit: x
finally
found at 1
-1
"
    );
    assert_eq!(out.code, 0);
}

/// An error escaping `main`: `Uncaught <Type>: <message>` on stderr, exit code 1, no leaks.
#[test]
fn uncaught_error_in_main() {
    let mut pb = PB::new();
    let t = pb.t;
    let ed = pb.declare();
    let et = pb.adt_ty(ed, vec![]);
    pb.set_def(
        ed,
        Def::Adt(adt(
            "BadThing",
            AdtKind::Struct,
            vec![("message", t.str, None)],
        )),
    );
    let mut f = FB::new("main", t.unit);
    f.throws = Some(et);
    let body = vec![
        se(print(vec![s("before", t)], t)),
        se(throw(
            adt_lit(ed, vec![concat(s("oh ", t), s("no", t), t)], et),
            t.never,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(
        (out.stdout.as_str(), out.stderr.as_str(), out.code),
        ("before\n", "Uncaught BadThing: oh no\n", 1)
    );
}

/// `finally` runs on normal exit, on a caught error and on `return`/`break` through it.
#[test]
fn finally_on_every_exit() {
    let mut pb = PB::new();
    let t = pb.t;
    let thrower = {
        let mut f = FB::new("boom", t.i64);
        f.throws = Some(t.str);
        let x = f.param("x", t.i64, PassMode::Copy);
        let body = vec![
            if_(
                cmp(B::Gt, f.cp(x), int(1, t.i64), t),
                vec![se(throw(s("big", t), t.never))],
                None,
            ),
            ret(Some(f.cp(x))),
        ];
        pb.add_fn(f.build(body))
    };
    let early = {
        let mut f = FB::new("early", t.i64);
        let owned = f.local("owned", t.str);
        let body = vec![
            let_(owned, concat(s("o", t), s("k", t), t)),
            try_(
                vec![ret(Some(int(5, t.i64)))],
                None,
                Some(vec![se(print(vec![s("fin early", t), f.bw(owned)], t))]),
            ),
            ret(Some(int(0, t.i64))),
        ];
        pb.add_fn(f.build(body))
    };
    let mut f = FB::new("main", t.unit);
    let i = f.local("i", t.i64);
    let e = f.local("e", t.str);
    let body = vec![
        let_(i, int(0, t.i64)),
        while_(
            None,
            boolean(true, t),
            vec![
                try_(
                    vec![
                        se(print(vec![call(thrower, vec![f.cp(i)], t.i64)], t)),
                        if_(
                            cmp(B::Eq, f.cp(i), int(1, t.i64), t),
                            vec![st(StmtKind::Break(None))],
                            None,
                        ),
                    ],
                    Some((Some(e), vec![se(print(vec![s("caught", t), f.bw(e)], t))])),
                    Some(vec![se(print(vec![s("fin", t), f.cp(i)], t))]),
                ),
                se(cassign(B::Add, f.bm(i), int(2, t.i64), t)),
                if_(
                    cmp(B::Gt, f.cp(i), int(3, t.i64), t),
                    vec![se(cassign(B::Sub, f.bm(i), int(3, t.i64), t))],
                    None,
                ),
            ],
            None,
        ),
        se(print(vec![call(early, vec![], t.i64)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(
        out.stdout,
        "0\nfin 0\ncaught big\nfin 2\n1\nfin 1\nfin early ok\n5\n"
    );
}
