//! Generator syntax: `function*`, generator methods (`*name()`, `static *name()`,
//! `*[Symbol.iterator]()`), `yield`, bare `yield` and `yield*`, and the errors for `yield`
//! used as a name or as an operand; async generators (`async function*`, `async *name()`) and
//! `for await`; generator function expressions and object literal methods (#424).

mod common;

use common::*;

fn function(m: &Module, i: usize) -> &FnDecl {
    match &m.items[i].kind {
        ItemKind::Function(f) => f,
        k => panic!("expected a function, got {k:?}"),
    }
}

#[test]
fn generator_declarations() {
    let m = parse_ok(
        "function* a(): Generator<i64> { yield 1; }
         export function *b(): Generator<i64> {}
         function c(): void {}",
    );
    assert!(function(&m, 0).sig.is_generator);
    assert_eq!(function(&m, 0).sig.name.name, "a");
    assert!(function(&m, 1).sig.is_generator);
    assert!(m.items[1].exported);
    assert!(!function(&m, 2).sig.is_generator);
}

#[test]
fn generator_methods() {
    let m = parse_ok(
        "class C {
           *items(): Generator<i64> { yield 1; }
           static *make(): Generator<i64> {}
           *[Symbol.iterator](): Iterator<i64> {}
           plain(): void {}
         }",
    );
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!("class")
    };
    let gens: Vec<(&str, bool, bool)> = c
        .methods
        .iter()
        .map(|m| {
            (
                m.decl.sig.name.name.as_str(),
                m.decl.sig.is_generator,
                m.is_static,
            )
        })
        .collect();
    assert_eq!(
        gens,
        [
            ("items", true, false),
            ("make", true, true),
            (SYMBOL_ITERATOR, true, false),
            ("plain", false, false),
        ]
    );
}

#[test]
fn yield_expressions() {
    check("yield 1", "(yield 1)");
    check("yield", "(yield)");
    check("yield* xs", "(yield* xs)");
    check("yield a + b", "(yield (+ a b))");
    check("x = yield y", "(= x (yield y))");
    check("f(yield)", "(call f [(yield)])");
    check("(yield 1)", "(paren (yield 1))");
}

#[test]
fn yield_errors() {
    let e = errors("function f() { const yield = 1; }");
    assert!(
        e.iter().any(|m| m.contains("`yield` is a reserved word")),
        "{e:?}"
    );
    let e = errors("function* f(): Generator<i64> { const a = 1 + yield 2; }");
    assert!(
        e.iter().any(|m| m.contains("wrap it in parentheses")),
        "{e:?}"
    );
    let e = errors("declare function* g(): Generator<i64>;");
    assert!(
        e.iter().any(|m| m.contains("cannot be a generator")),
        "{e:?}"
    );
    let e = errors("interface I { *items(): Generator<i64>; }");
    assert!(
        e.iter()
            .any(|m| m.contains("interface methods cannot be generators")),
        "{e:?}"
    );
}

#[test]
fn async_generators() {
    let m = parse_ok(
        "async function* a(): AsyncGenerator<i64> { yield await f(); }
         class C {
           async *items(): AsyncGenerator<i64> { yield 1; }
           static async *make(): AsyncGenerator<i64> {}
           async *[Symbol.asyncIterator](): AsyncIterator<i64> {}
         }",
    );
    let f = function(&m, 0);
    assert!(f.sig.is_generator && f.sig.is_async);
    let ItemKind::Class(c) = &m.items[1].kind else {
        panic!("class")
    };
    for meth in &c.methods {
        assert!(
            meth.decl.sig.is_generator && meth.decl.sig.is_async,
            "{meth:?}"
        );
    }
    assert_eq!(c.methods[2].decl.sig.name.name, SYMBOL_ASYNC_ITERATOR);
}

#[test]
fn for_await() {
    let m = parse_ok(
        "async function f() {
           for await (const x of xs) {}
           for (const y of ys) {}
           for await (let [a, b] of pairs()) {}
         }",
    );
    let flags: Vec<bool> = body(&m)
        .iter()
        .map(|s| match &s.kind {
            StmtKind::ForOf { is_await, .. } => *is_await,
            k => panic!("expected for...of, got {k:?}"),
        })
        .collect();
    assert_eq!(flags, [true, false, true]);
    let e = errors("async function f() { for await (let i = 0; i < 3; i++) {} }");
    assert!(
        e.iter().any(|m| m.contains("`for await` needs `of`")),
        "{e:?}"
    );
}

/// The initializer of the `i`-th module-level `const`.
fn init(m: &Module, i: usize) -> &Expr {
    match &m.items[i].kind {
        ItemKind::Var(v) => v.init.as_ref().expect("initializer"),
        k => panic!("expected a const, got {k:?}"),
    }
}

#[test]
fn function_expressions() {
    let m = parse_ok(
        "const a = function* (n: i64): Generator<i64> { yield n; };
         const b = function* named(): Generator<i64> {};
         const c = async function* (): AsyncGenerator<i64> { yield 1; };
         const d = function (x: i64): i64 { return x; };
         const e = f(function* (): Generator<i64> {}, 2);",
    );
    let sigs: Vec<(String, bool, bool, usize)> = (0..4)
        .map(|i| match &init(&m, i).kind {
            ExprKind::Function(f) => (
                f.sig.name.name.clone(),
                f.sig.is_generator,
                f.sig.is_async,
                f.sig.params.len(),
            ),
            k => panic!("expected a function expression, got {k:?}"),
        })
        .collect();
    assert_eq!(
        sigs,
        [
            ("".into(), true, false, 1),
            ("named".into(), true, false, 0),
            ("".into(), true, true, 0),
            ("".into(), false, false, 1),
        ]
    );
    assert_eq!(sx(init(&m, 4)), "(call f [(function* (0) {0 stmts}) 2])");
}

#[test]
fn object_literal_methods() {
    let m = parse_ok(
        "const a = { *[Symbol.iterator](): Generator<i64> { yield 1; } };
         const b = { [Symbol.iterator](): Iterator<i64> { return it; }, n: 1 };
         const c = { async *[Symbol.asyncIterator](): AsyncGenerator<i64> {} };
         const d = { size(): i64 { return 1; }, async: 2, get };",
    );
    let methods: Vec<Vec<(String, bool, bool)>> = (0..4)
        .map(|i| match &init(&m, i).kind {
            ExprKind::Object(props) => props
                .iter()
                .filter_map(|p| match p {
                    ObjectProp::Method(f) => {
                        Some((f.sig.name.name.clone(), f.sig.is_generator, f.sig.is_async))
                    }
                    _ => None,
                })
                .collect(),
            k => panic!("expected an object literal, got {k:?}"),
        })
        .collect();
    assert_eq!(
        methods,
        [
            vec![(SYMBOL_ITERATOR.into(), true, false)],
            vec![(SYMBOL_ITERATOR.into(), false, false)],
            vec![(SYMBOL_ASYNC_ITERATOR.into(), true, true)],
            vec![("size".into(), false, false)],
        ]
    );
    let ExprKind::Object(props) = &init(&m, 3).kind else {
        panic!("object")
    };
    assert!(matches!(&props[1], ObjectProp::KeyValue(k, _) if k.name == "async"));
    assert!(matches!(&props[2], ObjectProp::Shorthand(k) if k.name == "get"));
}
