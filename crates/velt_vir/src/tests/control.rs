//! Control flow, evaluation order, entry point, noreturn intrinsics and the calling convention.

use velt_sema::hir::{BinOp as B, Intrinsic, PassMode};

use super::builder::*;
use super::{lower_ok, run};
use crate::vir::{Linkage, Ty};

/// `console.log(i, i++)` — earlier operands are snapshotted before later side effects.
#[test]
fn argument_snapshot_before_side_effects() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let i = f.local("i", t.i64);
    let tmp = f.local("tmp", t.i64);
    let post_inc = bexpr(
        vec![
            let_(tmp, f.cp(i)),
            se(cassign(B::Add, f.bm(i), int(1, t.i64), t)),
        ],
        f.cp(tmp),
    );
    let body = vec![
        let_(i, int(0, t.i64)),
        se(print(vec![f.cp(i), post_inc, f.cp(i)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "0 0 1\n");
}

/// do-while with `continue`: sema puts `if (!c) break;` in the loop `step`.
#[test]
fn break_inside_step() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let k = f.local("k", t.i64);
    let step = bexpr(
        vec![if_(
            not(cmp(B::Gt, f.cp(k), int(0, t.i64), t)),
            vec![brk(None)],
            None,
        )],
        ex(
            velt_sema::hir::ExprKind::Lit(velt_sema::hir::Lit::Unit),
            t.unit,
        ),
    );
    let body = vec![
        let_(k, int(5, t.i64)),
        while_(
            None,
            boolean(true, t),
            vec![
                se(cassign(B::Sub, f.bm(k), int(1, t.i64), t)),
                if_(
                    cmp(B::Eq, bin(B::Rem, f.cp(k), int(2, t.i64)), int(0, t.i64), t),
                    vec![cont(None)],
                    None,
                ),
                se(print(vec![f.cp(k)], t)),
            ],
            Some(step),
        ),
        se(print(vec![s("done", t)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "3\n1\ndone\n");
}

#[test]
fn exit_and_panic_are_noreturn() {
    let mut pb = PB::new();
    let t = pb.t;
    let f = FB::new("main", t.unit);
    let body = vec![
        se(print(vec![s("a", t)], t)),
        se(intr(Intrinsic::Exit, vec![int(7, t.i32)], t.never)),
        se(print(vec![s("b", t)], t)),
    ];
    pb.add_main(f.build(body));
    let p = pb.finish();
    let v = lower_ok(&p);
    assert!(
        !v.to_string().contains("static#1"),
        "dead code must be pruned:\n{v}"
    );
    let out = run(&p);
    assert_eq!((out.stdout.as_str(), out.code), ("a\n", 7));

    let mut pb = PB::new();
    let t = pb.t;
    let f = FB::new("main", t.unit);
    let body = vec![se(intr(
        Intrinsic::Panic,
        vec![concat(s("bo", t), s("om", t), t)],
        t.never,
    ))];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!((out.stderr.as_str(), out.code), ("panic: boom\n", 101));
}

#[test]
fn main_returning_i32_and_print_to_stderr() {
    let mut pb = PB::new();
    let t = pb.t;
    let f = FB::new("main", t.i32);
    let body = vec![
        se(intr(
            Intrinsic::PrintErr,
            vec![s("err", t), boolean(false, t), flt(0.5, t.f64)],
            t.unit,
        )),
        ret(Some(int(42, t.i32))),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!((out.stderr.as_str(), out.code), ("err false 0.5\n", 42));
}

#[test]
fn signatures_are_scalar_only() {
    let v = lower_ok(&super::goldens::strings());
    for f in &v.funcs {
        assert!(f.params.iter().all(|t| t.is_scalar()));
        assert!(!matches!(f.ret, Ty::Agg(_)));
    }
    let greet = v.funcs.iter().find(|f| f.symbol == "_V5greet").unwrap();
    assert_eq!(greet.params, vec![Ty::Ptr, Ty::Ptr]);
    assert_eq!(greet.ret, Ty::Unit);
    assert_eq!(greet.linkage, Linkage::Internal);
    let main = v.funcs.iter().find(|f| f.symbol == "velt_main").unwrap();
    assert_eq!(
        (main.symbol.as_str(), main.linkage, main.ret),
        ("velt_main", Linkage::Export, Ty::I32)
    );
}

#[test]
fn lowering_is_deterministic() {
    for p in [
        super::goldens::strings(),
        super::goldens::control(),
        super::goldens::functions(),
    ] {
        assert_eq!(crate::lower(&p).to_string(), crate::lower(&p).to_string());
    }
}

#[test]
fn owned_and_borrowed_params_with_string_return() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("join", t.str);
    let a = f.param("a", t.str, PassMode::Owned);
    let b = f.param("b", t.str, PassMode::Borrow);
    let body = vec![
        if_(
            cmp(
                B::Eq,
                intr(Intrinsic::StrLen, vec![f.bw(b)], t.usize),
                int(0, t.usize),
                t,
            ),
            vec![ret(Some(f.mv(a)))],
            None,
        ),
        ret(Some(concat(f.bw(a), f.bw(b), t))),
    ];
    let join = pb.add_fn(f.build(body));
    let m = FB::new("main", t.unit);
    pb.add_main(m.build(vec![
        se(print(
            vec![call(join, vec![to_s(int(1, t.i64), t), s("", t)], t.str)],
            t,
        )),
        se(print(
            vec![call(join, vec![to_s(int(1, t.i64), t), s("-2", t)], t.str)],
            t,
        )),
    ]));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "1\n1-2\n");
}
