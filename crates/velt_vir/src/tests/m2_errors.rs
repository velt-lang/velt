//! Throwing functions (Result via out-pointer), propagation, try/catch/finally, options:
//! `tests/golden/m2/errors.velt` loaded with the real std, and hand-lowered HIR for the edges.

use velt_sema::hir::{AdtKind, BinOp as B, Def, PassMode, StmtKind};

use super::builder::*;
use super::builder_m2::*;
use super::run;

/// `tests/golden/m2/errors.velt`, which indexes an array (the out-of-bounds report prints the
/// index through std's hooks, #2197): loaded from source with the real std (std_src.rs).
#[test]
fn golden_errors() {
    let out = super::run_src(&super::golden_src("m2", "errors"));
    assert_eq!(out.stdout, super::m2_golden("errors"));
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
        ("before\n", "Uncaught BadThing { message: 'oh no' }\n", 1)
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
