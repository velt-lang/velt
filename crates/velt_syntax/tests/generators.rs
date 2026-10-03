//! Generator syntax: `function*`, generator methods (`*name()`, `static *name()`,
//! `*[Symbol.iterator]()`), `yield`, bare `yield` and `yield*`, and the errors for `yield`
//! used as a name or as an operand.

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
