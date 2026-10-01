//! Drop elaboration: every test runs in the interpreter, which fails on leaks, double frees and
//! frees of non-heap pointers.

use velt_sema::hir::{BinOp as B, DefId, Expr, Intrinsic, LogicOp, PassMode};

use super::builder::*;
use super::{lower_ok, run};

/// `function take(s: string /*owned*/) { console.log(s); }`
fn add_take(pb: &mut PB) -> DefId {
    let t = pb.t;
    let mut f = FB::new("take", t.unit);
    let p = f.param("s", t.str, PassMode::Owned);
    let body = vec![se(print(vec![s("took", t), f.bw(p)], t))];
    pb.add_fn(f.build(body))
}

fn heap_str(n: u128, t: T) -> Expr {
    to_s(int(n, t.i64), t)
}

fn func<'a>(v: &'a crate::vir::Program, sym: &str) -> &'a crate::vir::Function {
    v.funcs
        .iter()
        .find(|f| f.symbol == sym)
        .unwrap_or_else(|| panic!("no fn {sym}"))
}

fn has_flag(f: &crate::vir::Function) -> bool {
    f.locals
        .iter()
        .any(|l| l.name.as_deref().is_some_and(|n| n.ends_with(".dropflag")))
}

/// `function f(c: bool) { let s = String(12); if (c) take(s); console.log("end"); }`
#[test]
fn conditional_move_uses_drop_flag() {
    let mut pb = PB::new();
    let t = pb.t;
    let take = add_take(&mut pb);
    let mut f = FB::new("f", t.unit);
    let c = f.param("c", t.bool, PassMode::Copy);
    let sv = f.local("s", t.str);
    let body = vec![
        let_(sv, heap_str(12, t)),
        if_(f.cp(c), vec![se(call(take, vec![f.mv(sv)], t.unit))], None),
        se(print(vec![s("end", t)], t)),
    ];
    let fid = pb.add_fn(f.build(body));
    let m = FB::new("main", t.unit);
    pb.add_main(m.build(vec![
        se(call(fid, vec![boolean(true, t)], t.unit)),
        se(call(fid, vec![boolean(false, t)], t.unit)),
    ]));
    let p = pb.finish();
    let v = lower_ok(&p);
    assert!(has_flag(func(&v, "_V1f")), "expected a drop flag:\n{v}");
    let out = run(&p);
    assert_eq!(out.stdout, "took 12\nend\nend\n");
    assert_eq!(out.frees, 2);
}

/// Unconditional move: no flag, source not dropped, destination dropped once.
#[test]
fn unconditional_move_is_static() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let a = f.local("a", t.str);
    let b = f.local("b", t.str);
    let body = vec![
        let_(a, heap_str(1, t)),
        let_(b, f.mv(a)),
        se(print(vec![f.bw(b)], t)),
    ];
    pb.add_main(f.build(body));
    let p = pb.finish();
    let v = lower_ok(&p);
    assert!(!has_flag(func(&v, "_V4main")));
    let out = run(&p);
    assert_eq!(out.stdout, "1\n");
    assert_eq!(out.frees, 1);
}

/// Loop body locals are dropped on `break`, `continue` and normal iteration end.
#[test]
fn loop_exits_drop_body_locals() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let i = f.local("i", t.i64);
    let sv = f.local("s", t.str);
    let body = vec![
        let_(i, int(0, t.i64)),
        while_(
            None,
            boolean(true, t),
            vec![
                let_(sv, to_s(f.cp(i), t)),
                se(cassign(B::Add, f.cp(i), int(1, t.i64), t)),
                if_(
                    cmp(B::Eq, f.cp(i), int(2, t.i64), t),
                    vec![cont(None)],
                    None,
                ),
                if_(cmp(B::Eq, f.cp(i), int(4, t.i64), t), vec![brk(None)], None),
                se(print(vec![f.bw(sv)], t)),
            ],
            None,
        ),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "0\n2\n");
    assert_eq!(out.frees, 4);
}

/// Returning from nested scopes drops everything still owned; the returned value is moved out.
#[test]
fn return_from_nested_scope() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("g", t.str);
    let n = f.param("n", t.i64, PassMode::Copy);
    let a = f.local("a", t.str);
    let b = f.local("b", t.str);
    let body = vec![
        let_(a, to_s(f.cp(n), t)),
        sblock(vec![
            let_(b, concat(f.bw(a), s("x", t), t)),
            if_(
                cmp(B::Gt, f.cp(n), int(1, t.i64), t),
                vec![ret(Some(f.mv(b)))],
                None,
            ),
        ]),
        ret(Some(f.mv(a))),
    ];
    let g = pb.add_fn(f.build(body));
    let m = FB::new("main", t.unit);
    pb.add_main(m.build(vec![
        se(print(vec![call(g, vec![int(1, t.i64)], t.str)], t)),
        se(print(vec![call(g, vec![int(5, t.i64)], t.str)], t)),
    ]));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "1\n5x\n");
    assert_eq!(out.frees, 4);
}

/// Temporaries used only as borrows are dropped at the end of the statement.
#[test]
fn statement_temporaries_dropped() {
    let mut pb = PB::new();
    let t = pb.t;
    let f = FB::new("main", t.unit);
    let body = vec![
        se(print(vec![concat(heap_str(1, t), heap_str(2, t), t)], t)),
        se(print(
            vec![intr(
                Intrinsic::StrLen,
                vec![concat(heap_str(10, t), s("", t), t)],
                t.usize,
            )],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "12\n2\n");
    // Each concatenation with a `ToString` part is one builder, no intermediate strings (the
    // interpreter's builder reallocates on every push: 2 pushes + 1 push, 3 frees in all).
    assert_eq!(out.frees, 3);
}

/// Assignment drops the previous value (also `+=` on strings and assignment through a by-reference param).
#[test]
fn assignment_drops_old_value() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("bang", t.unit);
    let p = f.param("s", t.str, PassMode::BorrowMut);
    let body = vec![se(assign(f.bm(p), concat(f.bw(p), s("!", t), t), t))];
    let bang = pb.add_fn(f.build(body));

    let mut f = FB::new("main", t.unit);
    let sv = f.local("s", t.str);
    let body = vec![
        let_(sv, heap_str(1, t)),
        se(assign(f.bm(sv), heap_str(2, t), t)),
        se(assign(f.bm(sv), concat(f.bw(sv), s("x", t), t), t)),
        se(cassign(B::Add, f.bm(sv), heap_str(3, t), t)),
        se(call(bang, vec![f.bm(sv)], t.unit)),
        se(print(vec![f.bw(sv)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "2x3!\n");
}

/// `let s; if (c) s = a else s = b;` — conditionally initialized local gets a flag.
#[test]
fn uninit_let_conditional_assign() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("h", t.unit);
    let c = f.param("c", t.bool, PassMode::Copy);
    let sv = f.local("s", t.str);
    let unused = f.local("unused", t.str);
    let body = vec![
        let_uninit(unused),
        let_uninit(sv),
        if_(f.cp(c), vec![se(assign(f.bm(sv), heap_str(1, t), t))], None),
        se(print(vec![s("h", t)], t)),
    ];
    let h = pb.add_fn(f.build(body));
    let m = FB::new("main", t.unit);
    pb.add_main(m.build(vec![
        se(call(h, vec![boolean(true, t)], t.unit)),
        se(call(h, vec![boolean(false, t)], t.unit)),
    ]));
    let p = pb.finish();
    let v = lower_ok(&p);
    assert!(has_flag(func(&v, "_V1h")));
    let out = run(&p);
    assert_eq!(out.frees, 1);
}

/// Owned params are dropped by the callee; the caller doesn't drop what it moved in.
#[test]
fn owned_param_dropped_by_callee() {
    let mut pb = PB::new();
    let t = pb.t;
    let take = add_take(&mut pb);
    let mut f = FB::new("main", t.unit);
    let sv = f.local("s", t.str);
    let body = vec![
        se(call(take, vec![heap_str(7, t)], t.unit)),
        let_(sv, heap_str(8, t)),
        se(call(take, vec![f.mv(sv)], t.unit)),
        se(call(take, vec![s("lit", t)], t.unit)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "took 7\ntook 8\ntook lit\n");
    assert_eq!(out.frees, 2);
}

/// Owned param moved on conditionally inside the callee.
#[test]
fn owned_param_conditionally_moved() {
    let mut pb = PB::new();
    let t = pb.t;
    let take = add_take(&mut pb);
    let mut f = FB::new("maybe", t.unit);
    let p = f.param("s", t.str, PassMode::Owned);
    let c = f.param("c", t.bool, PassMode::Copy);
    let body = vec![if_(
        f.cp(c),
        vec![se(call(take, vec![f.mv(p)], t.unit))],
        None,
    )];
    let maybe = pb.add_fn(f.build(body));
    let m = FB::new("main", t.unit);
    pb.add_main(m.build(vec![
        se(call(maybe, vec![heap_str(1, t), boolean(true, t)], t.unit)),
        se(call(maybe, vec![heap_str(2, t), boolean(false, t)], t.unit)),
    ]));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "took 1\n");
    assert_eq!(out.frees, 2);
}

/// Value-producing `if` with owned branches, `&&` with temporaries in the rhs, block expressions.
#[test]
fn value_if_logical_and_block_exprs() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let c = f.local("c", t.bool);
    let k = f.local("k", t.str);
    let x = f.local("x", t.str);
    let body = vec![
        let_(c, boolean(true, t)),
        let_(k, ifx(f.cp(c), heap_str(1, t), heap_str(2, t))),
        se(print(
            vec![f.bw(k), ifx(not(f.cp(c)), heap_str(3, t), s("lit", t))],
            t,
        )),
        se(print(
            vec![logic(
                LogicOp::And,
                f.cp(c),
                cmp(
                    B::Eq,
                    intr(Intrinsic::StrLen, vec![heap_str(123, t)], t.usize),
                    int(3, t.usize),
                    t,
                ),
                t,
            )],
            t,
        )),
        se(print(
            vec![bexpr(
                vec![let_(x, heap_str(9, t))],
                concat(f.bw(x), s("!", t), t),
            )],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "1 lit\ntrue\n9!\n");
    assert_eq!(out.frees, 4);
}

/// Moving a local inside a loop body declared outside: flagged; reassigned each iteration.
#[test]
fn move_in_loop_with_reassign() {
    let mut pb = PB::new();
    let t = pb.t;
    let take = add_take(&mut pb);
    let mut f = FB::new("main", t.unit);
    let sv = f.local("s", t.str);
    let i = f.local("i", t.i64);
    let body = vec![
        let_(sv, heap_str(0, t)),
        let_(i, int(0, t.i64)),
        while_(
            None,
            cmp(B::Lt, f.cp(i), int(3, t.i64), t),
            vec![
                if_(
                    cmp(B::Eq, f.cp(i), int(1, t.i64), t),
                    vec![
                        se(call(take, vec![f.mv(sv)], t.unit)),
                        se(assign(f.bm(sv), s("re", t), t)),
                    ],
                    None,
                ),
                se(assign(f.bm(sv), to_s(f.cp(i), t), t)),
            ],
            Some(cassign(B::Add, f.cp(i), int(1, t.i64), t)),
        ),
        se(print(vec![f.bw(sv)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "took 0\n2\n");
}

/// A function whose result is discarded: the returned string is still dropped.
#[test]
fn discarded_string_result_dropped() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("mk", t.str);
    let n = f.param("n", t.i64, PassMode::Copy);
    let body = vec![ret(Some(to_s(f.cp(n), t)))];
    let mk = pb.add_fn(f.build(body));
    let m = FB::new("main", t.unit);
    pb.add_main(m.build(vec![se(call(mk, vec![int(4, t.i64)], t.str))]));
    let out = run(&pb.finish());
    assert_eq!(out.frees, 1);
}
