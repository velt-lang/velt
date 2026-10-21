//! Statements: declarations, control flow, labels, try/catch, nested items.

mod common;

use common::*;

#[test]
fn variable_declarations() {
    let m = parse_ok("function f() { const a = 1; let b: i64 = 2; let c; const { x, y: z, ...r } = o; let [p, q, ...s] = arr; let _ = g(); let [, a, , b] = t; }");
    let StmtKind::Var(holes) = &body(&m)[6].kind else {
        panic!()
    };
    assert_eq!(pat(&holes.pattern), "[_, a, _, b]");
    let s = body(&m);
    let v = |i: usize| match &s[i].kind {
        StmtKind::Var(v) => v.clone(),
        _ => panic!(),
    };
    assert_eq!(v(0).kind, VarKind::Const);
    assert_eq!(v(1).kind, VarKind::Let);
    assert_eq!(ty(v(1).ty.as_ref().unwrap()), "i64");
    assert!(v(2).init.is_none());
    assert_eq!(pat(&v(3).pattern), "{x: x, y: z, ...r}");
    assert_eq!(pat(&v(4).pattern), "[p, q, ...s]");
    assert_eq!(pat(&v(5).pattern), "_");
}

#[test]
fn if_without_braces_wraps_in_block() {
    let m = parse_ok("function f(n: i64): i64 { if (n < 2) return n; else n = 1; return 0; }");
    let StmtKind::If { cond, then, els } = &body(&m)[0].kind else {
        panic!()
    };
    assert_eq!(sx(cond), "(< n 2)");
    assert_eq!(then.stmts.len(), 1);
    assert!(matches!(then.stmts[0].kind, StmtKind::Return(Some(_))));
    assert_eq!(then.span, then.stmts[0].span);
    let els = els.as_ref().unwrap();
    let StmtKind::Block(b) = &els.kind else {
        panic!("{:?}", els.kind)
    };
    assert!(matches!(b.stmts[0].kind, StmtKind::Expr(_)));
}

#[test]
fn else_if_chain() {
    let m = parse_ok("function f() { if (a) { x(); } else if (b) { y(); } else { z(); } }");
    let StmtKind::If { els: Some(e1), .. } = &body(&m)[0].kind else {
        panic!()
    };
    let StmtKind::If { els: Some(e2), .. } = &e1.kind else {
        panic!()
    };
    assert!(matches!(e2.kind, StmtKind::Block(_)));
}

#[test]
fn loops() {
    let m = parse_ok(
        "function f() { while (i < 10) i++; do { k--; } while (k > 0); for (let j = 0; j < 5; j++) {} for (;;) {} for (i = 0; i < 2; i++) x(); for (const x of xs) {} for (let [k, v] of m) {} }",
    );
    let s = body(&m);
    let StmtKind::While { body: b, .. } = &s[0].kind else {
        panic!()
    };
    assert_eq!(b.stmts.len(), 1);
    assert!(matches!(s[1].kind, StmtKind::DoWhile { .. }));
    let StmtKind::For {
        init: Some(init),
        cond: Some(c),
        update: Some(u),
        ..
    } = &s[2].kind
    else {
        panic!()
    };
    assert!(matches!(init.kind, StmtKind::Var(_)));
    assert_eq!(sx(c), "(< j 5)");
    assert_eq!(sx(u), "(++post j)");
    assert!(matches!(
        s[3].kind,
        StmtKind::For {
            init: None,
            cond: None,
            update: None,
            ..
        }
    ));
    let StmtKind::For {
        init: Some(init), ..
    } = &s[4].kind
    else {
        panic!()
    };
    assert!(matches!(init.kind, StmtKind::Expr(_)));
    let StmtKind::ForOf {
        kind,
        pattern,
        iter,
        ..
    } = &s[5].kind
    else {
        panic!()
    };
    assert_eq!(*kind, VarKind::Const);
    assert_eq!(pat(pattern), "x");
    assert_eq!(sx(iter), "xs");
    let StmtKind::ForOf { pattern, .. } = &s[6].kind else {
        panic!()
    };
    assert_eq!(pat(pattern), "[k, v]");
}

#[test]
fn labels_break_continue() {
    let m = parse_ok("function f() { outer: for (;;) { inner: while (true) { break outer; continue inner; break; continue; } } }");
    let StmtKind::Labeled { label, body: b } = &body(&m)[0].kind else {
        panic!()
    };
    assert_eq!(label.name, "outer");
    let StmtKind::For { body: fb, .. } = &b.kind else {
        panic!()
    };
    let StmtKind::Labeled { body: wb, .. } = &fb.stmts[0].kind else {
        panic!()
    };
    let StmtKind::While { body: w, .. } = &wb.kind else {
        panic!()
    };
    assert!(matches!(&w.stmts[0].kind, StmtKind::Break(Some(l)) if l.name == "outer"));
    assert!(matches!(&w.stmts[1].kind, StmtKind::Continue(Some(l)) if l.name == "inner"));
    assert!(matches!(&w.stmts[2].kind, StmtKind::Break(None)));
    assert!(matches!(&w.stmts[3].kind, StmtKind::Continue(None)));
}

#[test]
fn return_throw_try_block_empty_items() {
    let m = parse_ok(
        "function f() { return; return 1; throw e; try { a(); } catch (e) { b(); } finally { c(); } try {} catch {} try {} finally {} { nested(); } ; function g() {} struct S { a: i32 } class C {} enum E { A } interface I {} type T = i32; async function h() {} }",
    );
    let s = body(&m);
    assert!(matches!(s[0].kind, StmtKind::Return(None)));
    assert!(matches!(s[1].kind, StmtKind::Return(Some(_))));
    assert!(matches!(s[2].kind, StmtKind::Throw(_)));
    let StmtKind::Try {
        catch: Some((Some(p), _)),
        finally: Some(_),
        ..
    } = &s[3].kind
    else {
        panic!()
    };
    assert_eq!(pat(p), "e");
    assert!(matches!(
        s[4].kind,
        StmtKind::Try {
            catch: Some((None, _)),
            finally: None,
            ..
        }
    ));
    assert!(matches!(
        s[5].kind,
        StmtKind::Try {
            catch: None,
            finally: Some(_),
            ..
        }
    ));
    assert!(matches!(s[6].kind, StmtKind::Block(_)));
    assert!(matches!(s[7].kind, StmtKind::Empty));
    for st in &s[8..] {
        assert!(matches!(st.kind, StmtKind::Item(_)), "{:?}", st.kind);
    }
    assert_eq!(s.len(), 15);
    assert!(errors("function f() { try {} }")[0].contains("expected `catch` or `finally`"));
}

/// `for` heads with comma lists are desugared (parser/for_loop.rs): declarations into a block
/// around the loop (a label stays on the loop), updates into an immediately called arrow.
#[test]
fn for_comma_lists() {
    let m = parse_ok(
        "function f() { l: for (let i = 0, j: usize = 3; i < j; i++, j--) {} for (a = 1, b = 2; ; ) {} }",
    );
    let s = body(&m);
    let StmtKind::Block(b) = &s[0].kind else {
        panic!()
    };
    assert_eq!(b.stmts.len(), 3);
    assert!(matches!(b.stmts[0].kind, StmtKind::Var(_)));
    assert!(matches!(b.stmts[1].kind, StmtKind::Var(_)));
    let StmtKind::Labeled { label, body: lb } = &b.stmts[2].kind else {
        panic!()
    };
    assert_eq!(label.name, "l");
    let StmtKind::For {
        init: None,
        update: Some(u),
        ..
    } = &lb.kind
    else {
        panic!()
    };
    let ExprKind::Call { callee, args, .. } = &u.kind else {
        panic!()
    };
    assert!(args.is_empty());
    assert!(matches!(callee.kind, ExprKind::Paren(_)));
    let StmtKind::Block(b) = &s[1].kind else {
        panic!()
    };
    assert!(matches!(b.stmts[0].kind, StmtKind::Expr(_)));
    assert!(matches!(b.stmts[2].kind, StmtKind::For { init: None, .. }));
}

/// `for (const k in o)` is a loop over `Object.keys(o)`, whose callee has an empty span.
#[test]
fn for_in_is_a_loop_over_object_keys() {
    let m = parse_ok("function f() { for (const k in o) { g(k); } for (let k in a.b) {} }");
    let s = body(&m);
    let StmtKind::ForOf {
        kind: VarKind::Const,
        pattern,
        iter,
        is_await: false,
        ..
    } = &s[0].kind
    else {
        panic!()
    };
    assert_eq!(pat(pattern), "k");
    assert_eq!(sx(iter), "(call (. Object keys) [o])");
    let ExprKind::Call { callee, args, .. } = &iter.kind else {
        panic!()
    };
    assert_eq!(callee.span.lo, callee.span.hi);
    assert_eq!(iter.span, args[0].span);
    assert!(matches!(
        s[1].kind,
        StmtKind::ForOf {
            kind: VarKind::Let,
            ..
        }
    ));
}

/// `for (k in o)` / `for (x of xs)` over a target declared elsewhere: a loop over a hidden
/// `const` whose body first assigns it to the target.
#[test]
fn for_in_and_of_take_targets() {
    for src in [
        "function f() { for (k in o) {} }",
        "function f() { for ([a, b] of xs) {} }",
        "function f() { for (x.y in o) {} }",
    ] {
        let m = parse_ok(src);
        let StmtKind::ForOf { pattern, body, .. } = &body(&m)[0].kind else {
            panic!("{src}")
        };
        assert!(
            matches!(&pattern.kind, PatternKind::Ident(i) if i.name == velt_syntax::FOR_TARGET)
        );
        assert!(matches!(
            &body.stmts[0].kind,
            StmtKind::Expr(Expr {
                kind: ExprKind::Assign { op: None, .. },
                ..
            })
        ));
    }
    let errs = errors("function f() { for (a, b of xs) {} }");
    assert!(errs[0].contains("one loop variable or target"), "{errs:?}");
}

#[test]
fn for_in_needs_a_declared_name() {
    let errs = errors("function f() { for (const [a, b] in o) {} }");
    assert!(errs[0].contains("a single name"), "{errs:?}");
    let errs = errors("async function f() { for await (const k in o) {} }");
    assert!(errs[0].contains("needs `of`"), "{errs:?}");
}

/// `"k" in o` is a relational operator anywhere, also in a `for (…;…;…)` condition and inside
/// the places JavaScript's grammar allows it in the loop's head (parentheses, call arguments,
/// template substitutions, array and object literals, index brackets, a conditional's
/// consequent, function bodies); `for (k in o)` stays the loop's `in`.
#[test]
fn in_operator_with_string_keys() {
    parse_ok(
        r#"function f() { const t = "a" in o && !("b" in o); const u = "c" in o ? 1 : 2; for (let i = 0; "a" in o; i++) {} for (x = ("a" in o); ;) {} for (g = () => { return "a" in o; }; ;) {} for (x = id("a" in o); x; x = false) {} for (s = `${"a" in o}`; ;) {} for (y = true ? "a" in o ? 1 : 2 : 3; ;) {} for (z = ["a" in o, { b: "a" in o }][r["a" in o ? 0 : 1]]; ;) {} }"#,
    );
}
