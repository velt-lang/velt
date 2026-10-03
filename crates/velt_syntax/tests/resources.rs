//! Resource management syntax: `using` / `await using` declarations and the symbol keys
//! `[Symbol.dispose]` / `[Symbol.asyncDispose]` as member names.

mod common;

use common::*;

fn var(s: &Stmt) -> &VarDecl {
    match &s.kind {
        StmtKind::Var(v) => v,
        k => panic!("expected a declaration, got {k:?}"),
    }
}

#[test]
fn using_declarations() {
    let m = parse_ok(
        "async function f() { using a = open(); await using b: Conn = await connect(); using(x); await using; }",
    );
    let s = body(&m);
    assert_eq!(var(&s[0]).kind, VarKind::Using);
    assert_eq!(pat(&var(&s[0]).pattern), "a");
    assert_eq!(var(&s[1]).kind, VarKind::AwaitUsing);
    assert_eq!(ty(var(&s[1]).ty.as_ref().unwrap()), "Conn");
    // Without a name after it, `using` is an ordinary identifier.
    assert!(matches!(s[2].kind, StmtKind::Expr(_)));
    assert!(matches!(s[3].kind, StmtKind::Expr(_)));
    assert_eq!(VarKind::AwaitUsing.keyword(), "await using");
}

#[test]
fn using_errors() {
    let e = errors("export using top = 1; function f() { using r; }");
    assert!(
        e.iter().any(|m| m.contains("only allowed inside a block")),
        "{e:?}"
    );
    assert!(
        e.iter()
            .any(|m| m.contains("a `using` declaration must be initialized")),
        "{e:?}"
    );
}

#[test]
fn symbol_keys_name_members() {
    let m = parse_ok(
        "class R { [Symbol.dispose]() {} async [Symbol.asyncDispose]() {} }
         interface D { [Symbol.dispose](): void; }
         function f(r: R) { r[Symbol.dispose](); r[i]; }",
    );
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!("class")
    };
    let names: Vec<&str> = c
        .methods
        .iter()
        .map(|m| m.decl.sig.name.name.as_str())
        .collect();
    assert_eq!(names, [SYMBOL_DISPOSE, SYMBOL_ASYNC_DISPOSE]);
    assert!(c.methods[1].decl.sig.is_async);
    let ItemKind::Interface(i) = &m.items[1].kind else {
        panic!("interface")
    };
    assert_eq!(i.methods[0].sig.name.name, SYMBOL_DISPOSE);
    let ItemKind::Function(f) = &m.items[2].kind else {
        panic!("function")
    };
    let StmtKind::Expr(call) = &f.body.stmts[0].kind else {
        panic!("call")
    };
    let ExprKind::Call { callee, .. } = &call.kind else {
        panic!("call")
    };
    assert!(
        matches!(&callee.kind, ExprKind::Member { prop, .. } if prop.name == SYMBOL_DISPOSE),
        "{:?}",
        callee.kind
    );
    let StmtKind::Expr(index) = &f.body.stmts[1].kind else {
        panic!("index")
    };
    assert!(matches!(index.kind, ExprKind::Index { .. }));
    let e = errors("class I { [Symbol.species]() {} }");
    assert!(
        e.iter()
            .any(|m| m.contains("`Symbol.species` is not supported")),
        "{e:?}"
    );
}

#[test]
fn iterator_symbol_keys_name_members() {
    let m = parse_ok(
        "class R implements Iterable<i64> { [Symbol.iterator](): Iterator<i64> { return new It(); } }
         interface A<T> { [Symbol.asyncIterator](): AsyncIterator<T>; }
         function f(r: R) { const it = r[Symbol.iterator](); }",
    );
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!("class")
    };
    assert_eq!(c.methods[0].decl.sig.name.name, SYMBOL_ITERATOR);
    let ItemKind::Interface(i) = &m.items[1].kind else {
        panic!("interface")
    };
    assert_eq!(i.methods[0].sig.name.name, SYMBOL_ASYNC_ITERATOR);
    let ItemKind::Function(f) = &m.items[2].kind else {
        panic!("function")
    };
    let StmtKind::Var(v) = &f.body.stmts[0].kind else {
        panic!("let: {:?}", f.body.stmts[0].kind)
    };
    let Some(init) = &v.init else { panic!("init") };
    let ExprKind::Call { callee, .. } = &init.kind else {
        panic!("call")
    };
    assert!(
        matches!(&callee.kind, ExprKind::Member { prop, .. } if prop.name == SYMBOL_ITERATOR),
        "{:?}",
        callee.kind
    );
}
