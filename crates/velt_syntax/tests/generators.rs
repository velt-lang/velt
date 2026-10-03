//! Generator syntax: `function*`, generator methods (`*name()`, `static *name()`,
//! `*[Symbol.iterator]()`), `yield`, bare `yield` and `yield*`, and the errors for `yield`
//! used as a name or as an operand; async generators (`async function*`, `async *name()`) and
//! `for await`.

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
