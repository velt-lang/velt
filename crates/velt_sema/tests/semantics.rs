//! Focused tests for resolution, typing, desugaring and ownership rules.

mod common;

use common::*;
use velt_sema::hir::{self, Callee, ExprKind as E, Intrinsic, StmtKind as S, UseMode};

fn main_fn(body: Vec<velt_syntax::ast::Stmt>) -> Vec<velt_syntax::ast::Item> {
    vec![func("main", &[], None, body)]
}

/// `let s = `x${1}`;` — an owned, non-literal string.
fn owned(name: &str) -> velt_syntax::ast::Stmt {
    let_(name, tpl(&["x", ""], vec![int(1)]))
}

// ───────────────────────────── ownership ─────────────────────────────

/// Strings are values: a copy in one branch leaves `a` usable after it.
#[test]
fn string_copy_in_one_branch_keeps_the_source() {
    ok(main_fn(vec![
        owned("a"),
        if_(bool_(true), vec![const_("b", var("a"))], None),
        es(log(vec![var("a")])),
    ]));
}

#[test]
fn move_in_both_branches_then_reassign_is_ok() {
    ok(main_fn(vec![
        owned("a"),
        if_(
            bool_(true),
            vec![const_("b", var("a"))],
            Some(block_s(vec![const_("c", var("a"))])),
        ),
        es(assign(var("a"), str_("new"))),
        es(log(vec![var("a")])),
    ]));
}

#[test]
fn move_in_diverging_branch_is_ok() {
    ok(main_fn(vec![
        owned("a"),
        if_(bool_(true), vec![const_("b", var("a")), ret_void()], None),
        es(log(vec![var("a")])),
    ]));
}

/// A string copied inside a loop is copied on every iteration (a move would be used again).
#[test]
fn string_copy_inside_loop_is_fine() {
    let p = ok(main_fn(vec![
        owned("a"),
        while_(
            bool_(true),
            vec![
                const_("b", var("a")),
                if_(bool_(false), vec![brk(None)], None),
            ],
        ),
    ]));
    let main = fn_named(&p, "main");
    let clones = exprs_of(main)
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                E::Call {
                    callee: Callee::Intrinsic(Intrinsic::Share),
                    ..
                }
            )
        })
        .count();
    assert_eq!(clones, 1);
}

#[test]
fn move_inside_loop_reinitialized_is_ok() {
    ok(main_fn(vec![
        owned("a"),
        while_(
            bool_(true),
            vec![
                const_("b", var("a")),
                es(assign(var("a"), str_("again"))),
                brk(None),
            ],
        ),
        es(log(vec![var("a")])),
    ]));
}

#[test]
fn borrow_params_and_use_modes() {
    let p = ok(vec![
        func(
            "len",
            &[("s", "string")],
            Some("number"),
            vec![ret(member(var("s"), "length"))],
        ),
        func(
            "main",
            &[],
            None,
            vec![
                owned("a"),
                es(log(vec![call("len", vec![var("a")])])),
                const_("b", var("a")),
                es(log(vec![bin(B::Eq, var("b"), str_("x1"))])),
            ],
        ),
    ]);
    let main = fn_named(&p, "main");
    assert_eq!(
        uses(main),
        vec![
            ("a".to_string(), UseMode::Borrow),
            ("a".to_string(), UseMode::Move),
            ("b".to_string(), UseMode::Borrow),
        ]
    );
    let len = fn_named(&p, "len");
    assert_eq!(len.params[0].mode, hir::PassMode::Borrow);
}

#[test]
fn reassigning_a_param_makes_it_owned() {
    // A local rebinding: the callee owns its value (callers reusing theirs pass a clone).
    let p = ok(vec![
        func(
            "g",
            &[("s", "string")],
            None,
            vec![es(assign(var("s"), str_("x")))],
        ),
        func("main", &[], None, vec![]),
    ]);
    let g = fn_named(&p, "g");
    assert_eq!(g.params[0].mode, hir::PassMode::Owned);
    assert!(g.body.locals[0].mutable);
}

#[test]
fn uninitialized_use_is_error() {
    let d = errs(main_fn(vec![
        let_t("x", "i64", None),
        if_(bool_(true), vec![es(assign(var("x"), int(1)))], None),
        es(log(vec![var("x")])),
    ]));
    assert!(has_err(&d, "possibly uninitialized variable `x`"), "{d:?}");
}

#[test]
fn last_use_of_a_string_moves_it() {
    // `const b = a` copies `a` while `a` is used afterwards, and moves it otherwise. (Real
    // source: soft moves are keyed by span, and hand-built ASTs share one.)
    let p = common::programs::ok_src(
        "function main() { let a = `x${1}`; let b = a; console.log(a, b); let c = b; console.log(c); }",
    );
    let modes = uses(fn_named(&p, "main"));
    assert!(
        modes.contains(&("b".to_string(), UseMode::Move)),
        "{modes:?}"
    );
    assert!(
        modes.contains(&("a".to_string(), UseMode::Borrow)),
        "{modes:?}"
    );
}

// ───────────────────────────── typing ─────────────────────────────

#[test]
fn literal_typing_and_ranges() {
    let d = errs(main_fn(vec![const_t("u", "u8", int(300))]));
    assert!(has_err(&d, "literal out of range for `u8`"), "{d:?}");
    ok(main_fn(vec![const_t("m", "i8", neg(int(128)))]));
    let d = errs(main_fn(vec![const_t("m", "i8", int(128))]));
    assert!(has_err(&d, "out of range"));
    // An integer literal in a float context is that float (JS numbers).
    let p = ok(main_fn(vec![const_t("f", "f64", int(1))]));
    let main = fn_named(&p, "main");
    assert_eq!(
        *p.types.kind(main.body.locals[0].ty),
        hir::TyKind::Float(hir::FloatTy::F64)
    );
    // Suffixes.
    let p = ok(main_fn(vec![
        const_("a", int_s(10, "u8")),
        const_("b", bin(B::Add, int(1), var("a"))),
    ]));
    let main = fn_named(&p, "main");
    assert_eq!(
        *p.types.kind(main.body.locals[1].ty),
        hir::TyKind::Int(hir::IntTy::U8)
    );
}

#[test]
fn no_implicit_conversions() {
    let d = errs(main_fn(vec![
        const_t("a", "i64", int(1)),
        const_("b", float(1.0)),
        es(log(vec![bin(B::Add, var("a"), var("b"))])),
    ]));
    assert!(has_err(&d, "mismatched types"));
    // A local declared from a literal (no declared type) is a number: it mixes with floats.
    ok(main_fn(vec![
        const_("a", int(1)),
        const_("b", float(1.0)),
        es(log(vec![bin(B::Add, var("a"), var("b"))])),
    ]));
    let d = errs(main_fn(vec![es(log(vec![bin(B::Add, str_("a"), int(1))]))]));
    assert!(has_err(&d, "mismatched types"));
    let d = errs(main_fn(vec![
        const_t("u", "u8", int(1)),
        es(log(vec![neg(var("u"))])),
    ]));
    assert!(has_err(&d, "cannot apply unary operator `-` to type `u8`"));
    let d = errs(main_fn(vec![es(log(vec![neg(str_("1"))]))]));
    assert!(has_err(
        &d,
        "cannot apply unary operator `-` to type `string`"
    ));
    let d = errs(main_fn(vec![es(log(vec![cast(str_("1"), "i64")]))]));
    assert!(has_err(&d, "cannot cast `string` as `i64`"));
}

#[test]
fn errors_are_collected_without_cascades() {
    let d = errs(main_fn(vec![
        const_("x", var("nope")),
        const_("z", bin(B::Add, var("x"), int(1))),
        es(log(vec![var("z"), var("other")])),
    ]));
    let msgs: Vec<_> = d.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        msgs,
        vec![
            "cannot find `nope` in this scope",
            "cannot find `other` in this scope"
        ]
    );
}

#[test]
fn call_checks() {
    let d = errs(vec![
        func("f", &[("a", "i64")], Some("i64"), vec![ret(var("a"))]),
        func(
            "main",
            &[],
            None,
            vec![
                es(call("f", vec![int(1), int(2)])),
                es(call("f", vec![str_("x")])),
            ],
        ),
    ]);
    assert!(
        has_err(
            &d,
            "function `f` takes 1 argument but 2 arguments were supplied"
        ),
        "{d:?}"
    );
    assert!(has_err(&d, "mismatched types"));
    let d = errs(main_fn(vec![es(call("missing", vec![]))]));
    assert!(has_err(&d, "cannot find `missing` in this scope"));
}

#[test]
fn builtins() {
    let p = ok(vec![func(
        "main",
        &[],
        Some("i32"),
        vec![
            if_(
                bool_(false),
                vec![es(call("panic", vec![str_("boom")]))],
                None,
            ),
            es(call_e(
                member(var("console"), "error"),
                vec![str_("e"), int(1), float(1.5), bool_(true)],
            )),
            es(call_e(member(var("process"), "exit"), vec![int(3)])),
        ],
    )]);
    let main = fn_named(&p, "main");
    let ex = exprs_of(main);
    assert!(ex.iter().any(|e| matches!(
        e.kind,
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Panic),
            ..
        }
    )));
    assert!(ex.iter().any(|e| matches!(
        e.kind,
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::PrintErr),
            ..
        }
    )));
    // process.exit diverges, so no `return` is needed in an i32 main; its arg is typed i32.
    assert!(ex.iter().any(
        |e| matches!(&e.kind, E::Call { callee: Callee::Intrinsic(Intrinsic::Exit), args }
        if *p.types.kind(args[0].ty) == hir::TyKind::Int(hir::IntTy::I32))
    ));
}

// ───────────────────────────── control flow ─────────────────────────────

#[test]
fn missing_return() {
    let d = errs(vec![
        func(
            "f",
            &[("a", "i64")],
            Some("i64"),
            vec![if_(bin(B::Gt, var("a"), int(0)), vec![ret(int(1))], None)],
        ),
        func("main", &[], None, vec![]),
    ]);
    assert!(
        has_err(&d, "must return a value of type `i64` on every path"),
        "{d:?}"
    );
    // if/else both return; while(true) without break diverges.
    ok(vec![
        func(
            "f",
            &[("a", "i64")],
            Some("i64"),
            vec![if_(
                bin(B::Gt, var("a"), int(0)),
                vec![ret(int(1))],
                Some(block_s(vec![ret(int(2))])),
            )],
        ),
        func("g", &[], Some("i64"), vec![while_(bool_(true), vec![])]),
        func("main", &[], None, vec![]),
    ]);
    // ... but with a break it does not.
    let d = errs(vec![
        func(
            "g",
            &[],
            Some("i64"),
            vec![while_(bool_(true), vec![brk(None)])],
        ),
        func("main", &[], None, vec![]),
    ]);
    assert!(has_err(&d, "on every path"));
}

#[test]
fn break_continue_labels() {
    let d = errs(main_fn(vec![brk(None)]));
    assert!(has_err(&d, "`break` outside of a loop"));
    let d = errs(main_fn(vec![while_(bool_(true), vec![cont(Some("nope"))])]));
    assert!(has_err(&d, "use of undeclared label `nope`"));
    ok(main_fn(vec![labeled(
        "l",
        while_(bool_(true), vec![while_(bool_(true), vec![brk(Some("l"))])]),
    )]));
}

#[test]
fn do_while_with_continue_uses_step() {
    let p = ok(main_fn(vec![
        let_("k", int(3)),
        do_while(
            vec![
                es(post_dec(var("k"))),
                if_(bool_(true), vec![cont(None)], None),
            ],
            bin(B::Gt, var("k"), int(0)),
        ),
    ]));
    let main = fn_named(&p, "main");
    let w = main.body.block.stmts.iter().find_map(|s| match &s.kind {
        S::While { step, .. } => Some(step),
        _ => None,
    });
    assert!(matches!(
        w,
        Some(Some(hir::Expr {
            kind: E::Block(_),
            ..
        }))
    ));
}

#[test]
fn update_expressions_as_values() {
    let p = ok(main_fn(vec![
        let_("i", int(0)),
        const_("a", post_inc(var("i"))),
        const_("b", pre_inc(var("i"))),
    ]));
    let main = fn_named(&p, "main");
    let inits: Vec<_> = main
        .body
        .block
        .stmts
        .iter()
        .filter_map(|s| match &s.kind {
            S::Let { init: Some(e), .. } => Some(e),
            _ => None,
        })
        .collect();
    // postfix: Block { Let tmp = i; i += 1 } value tmp
    let E::Block(b) = &inits[1].kind else {
        panic!("{:?}", inits[1])
    };
    assert!(matches!(b.stmts[0].kind, S::Let { .. }));
    assert!(matches!(&b.stmts[1].kind, S::Expr(e) if matches!(e.kind, E::CompoundAssign { .. })));
    assert!(b.value.is_some());
    // prefix: Block { i += 1 } value i
    let E::Block(b) = &inits[2].kind else {
        panic!()
    };
    assert_eq!(b.stmts.len(), 1);
    assert!(main.body.locals.iter().any(|l| l.name == "<postfix>"));
}

#[test]
fn scoping_and_shadowing() {
    ok(main_fn(vec![
        const_("x", int(1)),
        block_s(vec![const_("x", str_("shadow")), es(log(vec![var("x")]))]),
        es(log(vec![bin(B::Add, var("x"), int(1))])),
    ]));
    let d = errs(main_fn(vec![const_("x", int(1)), let_("x", int(2))]));
    assert!(has_err(&d, "cannot redeclare block-scoped variable `x`"));
    // Loop variables are not visible after the loop.
    let d = errs(main_fn(vec![
        for_(
            Some(let_("j", int(0))),
            Some(bin(B::Lt, var("j"), int(1))),
            Some(post_inc(var("j"))),
            vec![],
        ),
        es(log(vec![var("j")])),
    ]));
    assert!(has_err(&d, "cannot find `j` in this scope"));
}

#[test]
fn main_rules() {
    let d = errs(vec![func("notmain", &[], None, vec![])]);
    assert!(has_err(&d, "`main` function not found"));
    let d = errs(vec![func(
        "main",
        &[],
        Some("string"),
        vec![ret(str_("x"))],
    )]);
    assert!(has_err(&d, "`main` must return `void` or `i32`"));
    let d = errs(vec![func("main", &[("a", "i64")], None, vec![])]);
    assert!(has_err(&d, "must not take parameters"));
}

#[test]
fn library_root_needs_no_main_but_is_fully_checked() {
    let lib = velt_sema::CheckOptions {
        require_main: false,
    };
    let (p, d) = run_with(
        vec![func("helper", &[], Some("i64"), vec![ret(int(1))])],
        lib,
    );
    assert!(d.iter().all(|d| !d.is_error()), "{d:?}");
    assert_eq!(p.expect("library accepted").entry, None);
    // A body nothing calls is still type-checked.
    let (p, d) = run_with(
        vec![func("helper", &[], Some("i64"), vec![ret(str_("x"))])],
        lib,
    );
    assert!(p.is_none());
    assert!(!has_err(&d, "`main` function not found"));
    assert!(d.iter().any(|d| d.is_error()), "{d:?}");
    // A `main` that is there is still validated.
    let (_, d) = run_with(vec![func("main", &[("a", "i64")], None, vec![])], lib);
    assert!(has_err(&d, "must not take parameters"));
}

#[test]
fn const_compound_assign_and_update() {
    let d = errs(main_fn(vec![
        const_("x", int(1)),
        es(cassign(B::Add, var("x"), int(1))),
        es(post_inc(var("x"))),
    ]));
    assert_eq!(
        d.iter()
            .filter(|d| d.message == "cannot assign twice to const `x`")
            .count(),
        2
    );
}
