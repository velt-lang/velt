//! Decorators: every target and form, `declare decorator function`, and where they are refused.

mod common;

use common::*;

fn callee(d: &Decorator) -> String {
    let inner = match &d.expr.kind {
        ExprKind::Call { callee, .. } => callee,
        _ => &d.expr,
    };
    fn name(e: &Expr) -> String {
        match &e.kind {
            ExprKind::Ident(i) => i.name.clone(),
            ExprKind::Member { object, prop, .. } => format!("{}.{}", name(object), prop.name),
            ExprKind::Paren(_) => "(…)".into(),
            _ => "?".into(),
        }
    }
    name(inner)
}

fn names(ds: &[Decorator]) -> Vec<String> {
    ds.iter().map(callee).collect()
}

#[test]
fn on_every_target() {
    let m = parse_ok(
        "@a @b(1) function f(@c x: i64, @d() y: i64) {}
         @e @ns.deep.g(\"k\") class C {
           @h field: i64 = 1;
           @i m() {}
           @j get g(): i64 { return 1; }
           constructor(@k private readonly p: i64) {}
         }
         @l struct S { @m x: i64; }
         @n enum E { A }
         @o declare function ext(@p a: i64): void;
         extend i64 { @q twice(): i64 { return 2; } }",
    );
    let ItemKind::Function(f) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(names(&f.sig.decorators), ["a", "b"]);
    assert_eq!(names(&f.sig.params[0].decorators), ["c"]);
    assert_eq!(names(&f.sig.params[1].decorators), ["d"]);
    let ItemKind::Class(c) = &m.items[1].kind else {
        panic!()
    };
    assert_eq!(names(&c.decorators), ["e", "ns.deep.g"]);
    let field = c.fields.iter().find(|f| f.name.name == "field").unwrap();
    assert_eq!(names(&field.decorators), ["h"]);
    assert_eq!(names(&c.methods[0].decl.sig.decorators), ["i"]);
    assert_eq!(names(&c.methods[1].decl.sig.decorators), ["j"]);
    let ctor = c.constructor.as_ref().unwrap();
    assert_eq!(names(&ctor.sig.params[0].decorators), ["k"]);
    let p = c.fields.iter().find(|f| f.name.name == "p").unwrap();
    assert!(p.readonly && p.is_private && p.decorators.is_empty());
    let ItemKind::Struct(s) = &m.items[2].kind else {
        panic!()
    };
    assert_eq!(names(&s.decorators), ["l"]);
    assert_eq!(names(&s.fields[0].decorators), ["m"]);
    let ItemKind::Enum(e) = &m.items[3].kind else {
        panic!()
    };
    assert_eq!(names(&e.decorators), ["n"]);
    let ItemKind::ExternFn(x) = &m.items[4].kind else {
        panic!()
    };
    assert_eq!(names(&x.decorators), ["o"]);
    assert_eq!(names(&x.params[0].decorators), ["p"]);
    let ItemKind::Extend(x) = &m.items[5].kind else {
        panic!()
    };
    assert_eq!(names(&x.methods[0].decl.sig.decorators), ["q"]);
}

#[test]
fn either_side_of_export_and_inside_blocks() {
    let m = parse_ok("@a export class A {} export @b class B {} function f() { @c class C {} }");
    for (i, name) in [(0, "a"), (1, "b")] {
        let ItemKind::Class(t) = &m.items[i].kind else {
            panic!()
        };
        assert!(m.items[i].exported);
        assert_eq!(names(&t.decorators), [name]);
    }
    let ItemKind::Function(f) = &m.items[2].kind else {
        panic!()
    };
    let StmtKind::Item(item) = &f.body.stmts[0].kind else {
        panic!()
    };
    let ItemKind::Class(t) = &item.kind else {
        panic!()
    };
    assert_eq!(names(&t.decorators), ["c"]);
}

#[test]
fn the_item_span_starts_at_the_decorator() {
    let m = parse_ok("@a class C { @b f: i64 = 1; }");
    assert_eq!(m.items[0].span.lo, 0);
    let ItemKind::Class(t) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(t.fields[0].span.lo, "@a class C { ".len() as u32);
}

#[test]
fn a_parenthesized_expression_is_a_form() {
    let m = parse_ok("@(a.b[0]) class C {}");
    let ItemKind::Class(t) = &m.items[0].kind else {
        panic!()
    };
    assert!(matches!(t.decorators[0].expr.kind, ExprKind::Paren(_)));
}

#[test]
fn declare_decorator_function() {
    let m = parse_ok(
        "export declare decorator function name(key: string): FieldDecorator;
         declare function decorator(): void;
         function decorator2() { const decorator = 1; }",
    );
    let ItemKind::ExternFn(s) = &m.items[0].kind else {
        panic!()
    };
    assert!(s.is_decorator && !s.is_system && !s.is_unsafe && m.items[0].exported);
    let ItemKind::ExternFn(s) = &m.items[1].kind else {
        panic!()
    };
    assert!(
        !s.is_decorator,
        "`decorator` is a word only before `function`"
    );
}

#[track_caller]
fn refuses(src: &str, message: &str) {
    let found = errors(src);
    assert!(
        found.iter().any(|e| e.contains(message)),
        "`{src}`: expected `{message}`, got {found:?}"
    );
}

#[test]
fn refused_places() {
    let declaration = "a decorator cannot be applied to this declaration";
    refuses("@a const x = 1;", declaration);
    refuses("@a interface I {}", declaration);
    refuses("@a type T = i64;", declaration);
    refuses("@a import { x } from \"./y\";", declaration);
    refuses("@a export { x };", declaration);
    refuses("@a namespace N {}", declaration);
    refuses("@a extend i64 {}", declaration);
    // A variable holding a class is still a variable (TypeScript's TS1206).
    let binding = "a decorator cannot be applied to a variable, even one holding a class";
    refuses("@a const K = class {};", binding);
    refuses("export @a let K = class K {};", binding);
    refuses("function f() { @a const K = class {}; }", binding);
    refuses("@a export @b class C {}", "before `export` or after it");
    refuses("class C { @a constructor() {} }", "to a constructor");
    refuses("class C { @a static {} }", "to a static block");
    let parameter = "a decorator cannot be applied to this parameter";
    refuses("interface I { m(@a x: i64): void; }", parameter);
    refuses("const o = { *[Symbol.iterator](@a x: i64) {} };", parameter);
    refuses("const o = { m(@a x: i64) {} };", "found `@`");
    refuses("const g = function* (@a x: i64) {};", parameter);
    let overload = "parameter of an overload signature";
    refuses(
        "function f(@a x: i64): i64; function f(x: i64) {}",
        overload,
    );
    refuses("class C { m(@a x: i64): i64; m(x: i64) {} }", overload);
    refuses(
        "const x = @a class {};",
        "TC39 run-time decorators are not supported yet (#2204)",
    );
}

#[test]
fn a_parameter_list_inside_a_signature_starts_out_refused() {
    // The object type's method is in the generics of a declaration whose own parameters may
    // have decorators: each list answers for itself.
    refuses(
        "function f<T extends { m(@a x: i64): void }>(y: T) {}",
        "cannot be applied to this parameter",
    );
    parse_ok("function f<T extends { m(x: i64): void }>(@a y: T) {}");
}

#[test]
fn type_arguments_on_a_decorator_call() {
    let m = parse_ok("@fac<string, i64>(\"a\") class C { @ns.fac<Array<i64>>() m() {} }");
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!("not a class");
    };
    let ExprKind::Call {
        type_args, args, ..
    } = &c.decorators[0].expr.kind
    else {
        panic!("not a call");
    };
    assert_eq!((type_args.len(), args.len()), (2, 1));
    let ExprKind::Call { type_args, .. } = &c.methods[0].decl.sig.decorators[0].expr.kind else {
        panic!("not a call");
    };
    assert_eq!(type_args.len(), 1);
}

#[test]
fn type_arguments_without_a_call_are_refused() {
    let errs = errors("@fac<string> class C {}");
    assert!(
        errs.iter()
            .any(|e| e.contains("must be followed by its arguments")),
        "{errs:?}"
    );
}

/// The declaration of `export default …` in `src`, and whether it is exported itself (one with
/// no name is; one with a name comes with an export list).
fn default_declaration(src: &str) -> (Module, bool) {
    let m = parse_ok(src);
    let exported = m.items[0].exported;
    (m, exported)
}

#[test]
fn either_side_of_export_default() {
    for (src, want) in [
        ("export default @a class C {}", "C"),
        ("@a export default class C {}", "C"),
        ("export default @a class {}", "default"),
        ("@a export default class {}", "default"),
        ("export default @a class extends B {}", "default"),
        ("@a export default class extends B {}", "default"),
        ("export default @a abstract class C {}", "C"),
        ("@a export default abstract class C {}", "C"),
        ("export default @a struct S {}", "S"),
        ("@a export default struct S {}", "S"),
    ] {
        let (m, exported) = default_declaration(src);
        let (ItemKind::Class(t) | ItemKind::Struct(t)) = &m.items[0].kind else {
            panic!("{src}: {:?}", m.items[0].kind)
        };
        assert_eq!(names(&t.decorators), ["a"], "{src}");
        assert_eq!(t.name.name, want, "{src}");
        assert_eq!(exported, want == "default", "{src}");
        assert_eq!(m.items.len(), if exported { 1 } else { 2 }, "{src}");
    }
    let (m, _) = default_declaration("export default @a enum E { A }");
    let ItemKind::Enum(e) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(names(&e.decorators), ["a"]);
    for src in [
        "export default @a function f() {}",
        "@a export default function f() {}",
        "export default @a function () {}",
        "@a export default async function f() {}",
    ] {
        let (m, _) = default_declaration(src);
        let ItemKind::Function(f) = &m.items[0].kind else {
            panic!()
        };
        assert_eq!(names(&f.sig.decorators), ["a"], "{src}");
    }
    // Its span starts at what is written first, and its export list stays a list.
    let m = parse_ok("@a export default class C {}");
    assert_eq!(m.items[0].span.lo, 0);
    assert!(matches!(&m.items[1].kind, ItemKind::Import(i) if velt_syntax::written_default(i)));
}

#[test]
fn refused_around_export_default() {
    refuses(
        "@a export default @b class C {}",
        "before `export` or after it",
    );
    let declaration = "a decorator cannot be applied to this declaration";
    refuses("export default @a interface I {}", declaration);
    refuses("@a export default interface I {}", declaration);
    let expression = "a decorator cannot be applied to an expression";
    refuses("const x = 1; export default @a x;", expression);
    refuses("const x = 1; @a export default x;", expression);
    refuses("@a export default 1 + 2;", expression);
    // A decorator on a class expression is still that, in parentheses.
    refuses(
        "export default (@a class {});",
        "TC39 run-time decorators are not supported yet (#2204)",
    );
}

#[test]
fn a_functions_decorators_are_its_implementations() {
    for src in [
        // Before the first signature, on either side of `export`.
        "@a export default function f(x: string): string; export default function f(x: any): any {}",
        "export default @a function f(x: string): string; export default function f(x: any): any {}",
        // On the implementation, on either side of `export`.
        "export default function f(x: string): string; @a export default function f(x: any): any {}",
        "export default function f(x: string): string; export default @a function f(x: any): any {}",
        // With no name, and on a signature between.
        "@a export default function (x: string): string; export default function (x: any): any {}",
        "export default function (x: string): string; @a export default function (x: any): any {}",
        "export default function f(x: string): string; @a export default function f(x: i64): i64; export default function f(x: any): any {}",
        // Not a default.
        "@a function f(x: string): string; function f(x: any): any {}",
        "function f(x: string): string; @a function f(x: any): any {}",
        "@a export function f(x: string): string; export function f(x: any): any {}",
        "export @a function f(x: string): string; export function f(x: any): any {}",
        "export function f(x: string): string; @a export function f(x: any): any {}",
        "export function f(x: string): string; export @a function f(x: any): any {}",
    ] {
        let m = parse_ok(src);
        let ItemKind::Function(f) = &m.items[0].kind else {
            panic!()
        };
        assert_eq!(names(&f.sig.decorators), ["a"], "{src}");
        assert!(!f.overloads.is_empty(), "{src}");
        for o in &f.overloads {
            assert!(o.decorators.is_empty(), "{src}");
        }
    }
    // In the order written, the first signature's first.
    let m = parse_ok("@a function f(x: string): string; @b function f(x: any): any {}");
    let ItemKind::Function(f) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(names(&f.sig.decorators), ["a", "b"]);
    refuses(
        "export function f(x: string): string; @a export @b function f(x: any): any {}",
        "before `export` or after it",
    );
    // A signature that says `export` while the next doesn't is still TS2383, decorated or not.
    let overloads = "overload signatures must all be exported";
    refuses(
        "@a export function f(x: string): string; function f(x: any): any {}",
        overloads,
    );
    refuses(
        "function f(x: string): string; @a export function f(x: any): any {}",
        overloads,
    );
    refuses(
        "export function f(x: string): string; @a function f(x: any): any {}",
        overloads,
    );
    // A decorator after a signature doesn't stand for its implementation.
    refuses(
        "function f(x: string): string; @a function g(x: any): any {}",
        "",
    );
    refuses("function f(x: string): string; @a const y = 1;", "");
}
