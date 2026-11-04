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
    assert!(s.is_decorator && !s.is_system && !s.is_intrinsic && m.items[0].exported);
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
