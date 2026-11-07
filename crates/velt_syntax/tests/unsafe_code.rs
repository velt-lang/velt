//! `unsafe` blocks, expressions and functions (docs/internals/design/c-ffi.md).

mod common;

use common::*;

fn first_fn(m: &Module) -> &FnDecl {
    m.items
        .iter()
        .find_map(|i| match &i.kind {
            ItemKind::Function(f) => Some(f),
            _ => None,
        })
        .expect("a function")
}

#[test]
fn unsafe_block_statement() {
    let m = parse_ok("function f() { unsafe { g(); h(); } }");
    let StmtKind::Unsafe(b) = &body(&m)[0].kind else {
        panic!("{:?}", body(&m)[0].kind)
    };
    assert_eq!(b.stmts.len(), 2);
}

#[test]
fn unsafe_expression_is_its_expression() {
    let m = parse_ok("function f() { const rc = unsafe { step(s) + 1 }; }");
    let StmtKind::Var(v) = &body(&m)[0].kind else {
        panic!()
    };
    assert_eq!(
        sx(v.init.as_ref().unwrap()),
        "(unsafe (+ (call step [s]) 1))"
    );
}

#[test]
fn unsafe_expression_in_an_arrow_body() {
    let m = parse_ok("const release = (p: i64) => unsafe { finalize(p) };");
    assert_eq!(m.items.len(), 1);
}

#[test]
fn unsafe_expression_holds_one_expression() {
    let e = errors("function f() { const x = unsafe { a(); b() }; }");
    assert!(
        e.iter().any(|m| m.contains("holds one expression")),
        "{e:?}"
    );
}

#[test]
fn unsafe_functions() {
    let m = parse_ok("unsafe function f(p: i64): i64 { return p; }");
    let f = first_fn(&m);
    assert!(f.sig.is_unsafe && !f.sig.is_async);
    let m = parse_ok("export unsafe async function g(): Promise<void> {}");
    let f = first_fn(&m);
    assert!(f.sig.is_unsafe && f.sig.is_async);
    assert!(m.items[0].exported);
    let m = parse_ok("function outer() { unsafe function inner() {} inner(); }");
    let StmtKind::Item(i) = &body(&m)[0].kind else {
        panic!()
    };
    let ItemKind::Function(inner) = &i.kind else {
        panic!()
    };
    assert!(inner.sig.is_unsafe);
    assert!(!first_fn(&parse_ok("function plain() {}")).sig.is_unsafe);
}

#[test]
fn unsafe_methods() {
    let m = parse_ok("class C { unsafe raw(): i64 { return 0; } static unsafe make(): i64 { return 1; } safe() {} }");
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!()
    };
    let flags: Vec<(String, bool)> = c
        .methods
        .iter()
        .map(|m| (m.decl.sig.name.name.clone(), m.decl.sig.is_unsafe))
        .collect();
    assert_eq!(
        flags,
        [
            ("raw".to_string(), true),
            ("make".to_string(), true),
            ("safe".to_string(), false)
        ]
    );
}

#[test]
fn a_c_function_has_one_option() {
    for (src, says) in [
        (
            "declare c(\"c\", { errno: false }) function f(): i32;\n",
            "leaves it out",
        ),
        (
            "declare c(\"c\", { other: true }) function f(): i32;\n",
            "the only option",
        ),
        ("declare c(\"c\", errno) function f(): i32;\n", "{"),
    ] {
        let e = errors(src);
        assert!(e.iter().any(|m| m.contains(says)), "{src}: {e:?}");
    }
}

#[test]
fn unsafe_field_is_an_error() {
    let e = errors("class C { unsafe x: i64 = 0; }");
    assert!(
        e.iter()
            .any(|m| m.contains("`unsafe` are not allowed on fields")),
        "{e:?}"
    );
}

#[test]
fn unsafe_stays_an_identifier() {
    // A variable, a call, a property and a member named `unsafe`; a line break after the
    // word ends it (no `unsafe {` across lines).
    parse_ok("function f() { const unsafe = 1; g(unsafe); unsafe(2); o.unsafe = 3; }");
    parse_ok("class C { unsafe = 0; unsafe2() {} }");
    let m = parse_ok("function f() { unsafe\n{ g(); } }");
    assert!(
        matches!(body(&m)[0].kind, StmtKind::Expr(_)),
        "{:?}",
        body(&m)[0].kind
    );
}

#[test]
fn overloads_agree_on_unsafe() {
    let m = parse_ok("unsafe function f(a: i64): i64;\nunsafe function f(a: string): i64;\nunsafe function f(a: i64 | string): i64 { return 0; }");
    let f = first_fn(&m);
    assert!(f.sig.is_unsafe && f.overloads.iter().all(|o| o.is_unsafe));
    let e = errors("unsafe function f(a: i64): i64;\nfunction f(a: i64): i64 { return a; }");
    assert!(
        e.iter()
            .any(|m| m.contains("must all be `unsafe` or all be safe")),
        "{e:?}"
    );
    let e = errors("class C { unsafe m(a: i64): void;\n m(a: i64) {} }");
    assert!(
        e.iter()
            .any(|m| m.contains("must all be `unsafe` or all be safe")),
        "{e:?}"
    );
}

#[test]
fn implicitly_called_methods_cant_be_unsafe() {
    let e = errors("class C { unsafe [Symbol.dispose]() {} }");
    assert!(
        e.iter()
            .any(|m| m.contains("can't be unsafe: the language calls it implicitly")),
        "{e:?}"
    );
    let e = errors("abstract class C { abstract unsafe x: i64; }");
    assert!(
        e.iter()
            .any(|m| m.contains("`unsafe` cannot be used with `abstract`")),
        "{e:?}"
    );
}

#[test]
fn interface_members_cant_be_unsafe() {
    let e = errors("interface I { unsafe m(): void }");
    assert!(
        e.iter()
            .any(|m| m.contains("interface members can't be `unsafe`")),
        "{e:?}"
    );
}

#[test]
fn c_declarations() {
    let m = parse_ok("declare c(\"c\") function labs(x: i64): i64;\nexport declare c(\"m\") function cos(x: f64): f64;");
    let sigs: Vec<(String, String)> = m
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::ExternFn(s) => Some((s.name.name.clone(), s.c.as_ref()?.library.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        sigs,
        [("labs".into(), "c".into()), ("cos".into(), "m".into())]
    );
    assert!(m.items[1].exported);
    let asked = |src: &str| match &parse_ok(src).items[0].kind {
        ItemKind::ExternFn(s) => s.c.as_ref().is_some_and(|c| c.errno),
        _ => panic!("not a function"),
    };
    assert!(!asked("declare c(\"c\") function f(): i32;\n"));
    assert!(asked(
        "declare c(\"c\", { errno: true }) function f(): i32;\n"
    ));
    assert!(asked(
        "declare c(\"c\", { errno: true, }, ) function f(): i32;\n"
    ));
    // `c` is a word only there.
    parse_ok("function f() { const c = 1; c; }\ndeclare function c(x: i64): i64;");
    let e = errors("declare c(m) function f(): void;");
    assert!(
        e.iter()
            .any(|m| m.contains("the C library's name as a string")),
        "{e:?}"
    );
}

#[test]
fn c_types() {
    let m =
        parse_ok("declare c const A: i32;\nexport declare c const B: Ptr<void> | null\nlet c = 1;");
    assert!(m.items[1].exported);
    let ItemKind::Var(a) = &m.items[0].kind else {
        panic!("{:?}", m.items[0].kind)
    };
    assert!(a.c_const && a.ty.is_some() && a.init.is_none() && a.kind == VarKind::Const);
    let ItemKind::Var(c) = &m.items[2].kind else {
        panic!("{:?}", m.items[2].kind)
    };
    assert!(!c.c_const);
    let e = errors("declare c const A: i32 = 5;");
    assert!(e[0].contains("it has no initializer"), "{e:?}");
    let e = errors("declare c const A;");
    assert!(e[0].contains("a C constant says its type"), "{e:?}");
    let m = parse_ok("declare c type sqlite3;\nexport declare c type sqlite3_stmt;");
    let names: Vec<(String, bool)> = m
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Class(c) => Some((c.name.name.clone(), c.c_type)),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        [("sqlite3".into(), true), ("sqlite3_stmt".into(), true)]
    );
    assert!(m.items[1].exported);
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(c.ctor_visibility, CtorVisibility::Private);
    assert!(c.fields.is_empty() && c.methods.is_empty());
}

#[test]
fn c_structs() {
    let m = parse_ok(
        "declare c struct CGPoint { x: f64; y: f64; }\nexport declare c struct CGRect { origin: CGPoint; size: [f64, f64] }\nstruct Plain { x: f64 }",
    );
    let shapes: Vec<(String, bool, usize)> = m
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Struct(s) => Some((s.name.name.clone(), s.c_struct, s.fields.len())),
            _ => None,
        })
        .collect();
    assert_eq!(
        shapes,
        [
            ("CGPoint".into(), true, 2),
            ("CGRect".into(), true, 2),
            ("Plain".into(), false, 1)
        ]
    );
    assert!(m.items[1].exported && !m.items[0].exported);
    // `c` is a word only after `declare`: a struct named `c` is still a struct.
    let m = parse_ok("struct c { x: f64 }");
    assert!(matches!(&m.items[0].kind, ItemKind::Struct(s) if !s.c_struct));
}
