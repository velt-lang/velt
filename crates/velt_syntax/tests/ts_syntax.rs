//! Newer TypeScript syntax: `satisfies`, `let x!: T`, `const` type parameters, tuple labels,
//! several declarators, `declare` fields, `readonly` array types, type predicates, method
//! signatures in object types and methods in object literals.

mod common;

use common::*;

fn var(item: &Item) -> &VarDecl {
    match &item.kind {
        ItemKind::Var(v) => v,
        k => panic!("expected a variable, got {k:?}"),
    }
}

fn alias_ty(m: &Module, i: usize) -> String {
    match &m.items[i].kind {
        ItemKind::TypeAlias(a) => ty(&a.ty),
        k => panic!("expected a type alias, got {k:?}"),
    }
}

#[test]
fn satisfies_is_a_call_of_the_prelude_function() {
    let e = expr("x = { a: 1 } satisfies T");
    let ExprKind::Assign { value, .. } = &e.kind else {
        panic!("expected an assignment")
    };
    let ExprKind::Call {
        callee, type_args, ..
    } = &value.kind
    else {
        panic!("expected a call, got {:?}", value.kind)
    };
    assert!(matches!(&callee.kind, ExprKind::Ident(i) if i.name == "__satisfies"));
    assert_eq!(ty(&type_args[0]), "T");
}

#[test]
fn several_declarators_and_definite_assignment() {
    let m = parse_ok("export let a: number, b = 2, c!: string;");
    assert_eq!(m.items.len(), 3);
    assert!(m.items.iter().all(|i| i.exported));
    assert_eq!(ty(var(&m.items[2]).ty.as_ref().unwrap()), "string");
    let m = parse_ok("function f() { let i: number, j = 1; const k = 2, l = 3; }");
    assert_eq!(body(&m).len(), 4);
}

#[test]
fn const_type_parameters_and_tuple_labels() {
    let m = parse_ok("function f<T, const K>(x: K): T[] {} type W = [kind: string, b64: string];");
    let ItemKind::Function(f) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(f.sig.generics[1].name.name, "K");
    assert_eq!(alias_ty(&m, 1), "[string, string]");
}

#[test]
fn declare_fields() {
    let m = parse_ok("class E { declare readonly cause?: Error; }");
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(c.fields[0].name.name, "cause");
    assert!(c.fields[0].readonly && c.fields[0].default.is_none());
    assert_eq!(
        errors("class E { declare x: number = 1; }"),
        vec!["initializers are not allowed in ambient contexts"]
    );
}

#[test]
fn readonly_array_and_tuple_types() {
    let m = parse_ok(
        "type A = readonly number[]; type B = readonly [string, number]; type C = readonly (readonly [string, V])[];",
    );
    assert_eq!(alias_ty(&m, 0), "ReadonlyArray<number>");
    assert_eq!(alias_ty(&m, 1), "[string, number]");
    assert_eq!(alias_ty(&m, 2), "ReadonlyArray<[string, V]>");
    assert_eq!(
        errors("type R = readonly string;"),
        vec!["'readonly' type modifier is only permitted on array and tuple literal types"]
    );
}

#[test]
fn type_predicates() {
    let m = parse_ok(
        "function a(x: unknown): x is Foo { return true; } function b(x: number): asserts x is Foo {} function c(x: number): asserts x {} type F = (x: unknown) => x is Foo;",
    );
    let rets: Vec<String> = m.items[..3]
        .iter()
        .map(|i| match &i.kind {
            ItemKind::Function(f) => ty(f.sig.ret.as_ref().unwrap()),
            _ => panic!(),
        })
        .collect();
    assert_eq!(rets, vec!["x is Foo", "asserts x is Foo", "asserts x"]);
    assert_eq!(alias_ty(&m, 3), "fn(unknown) => x is Foo");
    parse_ok("const g = (n: Node | null): n is Element => n !== null;");
}

#[test]
fn method_signatures_in_object_types_and_optional_interface_methods() {
    let m = parse_ok(
        "type S = { stop(): void; size(n: number): number; label?(p: string): string }; interface H { q?(s: string): string | null; run(x: number): number }",
    );
    assert_eq!(
        alias_ty(&m, 0),
        "{stop: fn() => void; size: fn(number) => number; label: (fn(string) => string | null)}"
    );
    let ItemKind::Interface(i) = &m.items[1].kind else {
        panic!()
    };
    assert_eq!(i.fields[0].name.name, "q");
    assert!(i.fields[0].optional);
    assert_eq!(i.methods[0].sig.name.name, "run");
    assert_eq!(
        errors("type G = { run<T>(f: () => T): T };"),
        vec!["generic method signatures are only supported in interfaces: declare `run` in an interface"]
    );
    assert_eq!(
        errors("interface I { wrap?<A>(a: A): A }"),
        vec!["an optional method signature can't be generic yet: `wrap?` is a field of function type, and function types have no type parameters"]
    );
}

#[test]
fn object_literal_methods_are_arrow_properties() {
    let e = expr("x = { get(obj, prop) { return 1; }, async load(n: number) { return n; } }");
    let ExprKind::Assign { value, .. } = &e.kind else {
        panic!()
    };
    let ExprKind::Object(ps) = &value.kind else {
        panic!()
    };
    for p in ps {
        let ObjectProp::KeyValue(_, v) = p else {
            panic!("expected a property, got {p:?}")
        };
        assert!(matches!(v.kind, ExprKind::Arrow { .. }));
    }
    assert_eq!(
        errors("const o = { n: 1, m() { return this.n; } };"),
        vec!["`this` in the object-literal method `m` is not supported yet"]
    );
    assert!(errors("const o = { get v() { return 1; }, set v(x: number) {} };").is_empty());
    assert_eq!(
        errors("const o = { get v(x) { return 1; } };"),
        vec!["a getter cannot have parameters"]
    );
    assert_eq!(
        errors("const o = { set v() {} };"),
        vec!["a setter must have exactly one parameter"]
    );
}

#[test]
fn object_literal_accessors_are_calls_of_the_prelude_functions() {
    let e = expr("x = { get v(): number { return 1; }, set v(n: number) {} }");
    let ExprKind::Assign { value, .. } = &e.kind else {
        panic!("not an assignment: {e:?}");
    };
    let ExprKind::Object(ps) = &value.kind else {
        panic!()
    };
    let names: Vec<&str> = ps
        .iter()
        .map(|p| {
            let ObjectProp::KeyValue(k, v) = p else {
                panic!("expected a property, got {p:?}")
            };
            assert_eq!(k.name, "v");
            let ExprKind::Call { callee, args, .. } = &v.kind else {
                panic!("expected a call, got {v:?}")
            };
            assert!(matches!(args[0].kind, ExprKind::Arrow { .. }));
            match &callee.kind {
                ExprKind::Ident(f) => f.name.as_str(),
                k => panic!("expected a name, got {k:?}"),
            }
        })
        .collect();
    assert_eq!(names, [OBJECT_GET_FN, OBJECT_SET_FN]);
}

#[test]
fn computed_keys_are_spreads_of_the_prelude_function() {
    let e = expr("x = { a: 1, [o.tag]: 2, [\"b\"]: 3, [7]: 4 }");
    let ExprKind::Assign { value, .. } = &e.kind else {
        panic!("not an assignment: {e:?}");
    };
    let ExprKind::Object(props) = &value.kind else {
        panic!("not an object literal");
    };
    let ObjectProp::Spread(call) = &props[1] else {
        panic!("not a spread: {:?}", props[1]);
    };
    let ExprKind::Call { callee, args, .. } = &call.kind else {
        panic!("not a call");
    };
    assert!(matches!(&callee.kind, ExprKind::Ident(i) if i.name == COMPUTED_KEY_FN));
    assert_eq!(args.len(), 2);
    assert!(matches!(&props[2], ObjectProp::KeyValue(k, _) if k.name == "b"));
    assert!(matches!(&props[3], ObjectProp::KeyValue(k, _) if k.name == "7"));
    assert_eq!(
        errors("const o = { [a.b]() { return 1; } };"),
        vec!["a method name in an object literal must be a well-known symbol (`[Symbol.iterator]`) or a module constant (`[KEY]`), not an expression"]
    );
}

#[test]
fn import_meta_and_dynamic_import() {
    let m = parse_ok("const u = import.meta.url;\nfunction f() { return import.meta; }");
    let last = m.items.last().expect("an item");
    let ItemKind::Var(v) = &last.kind else {
        panic!("not a constant: {last:?}");
    };
    assert!(matches!(&v.pattern.kind, PatternKind::Ident(n) if n.name == IMPORT_META));
    assert_eq!(parse_ok("const u = 1;").items.len(), 1);
    let none: Vec<String> = vec![];
    assert_eq!(
        errors("const u = typeof import.meta.url === \"string\" ? import.meta.url : \"\";"),
        none
    );
    assert_eq!(
        errors("const m = import(\"./m\");\nconst n = import(x).then((y) => y);"),
        vec![
            "dynamic `import()` is not supported",
            "dynamic `import()` is not supported"
        ]
    );
}

#[test]
fn declare_global_and_module_blocks() {
    let m = parse_ok(
        "declare global {\n  interface W { a: number }\n}\ndeclare module \"./m\" {\n  export interface O { b?: string }\n}",
    );
    let ItemKind::Augment(g) = &m.items[0].kind else {
        panic!("not an augmentation: {:?}", m.items[0]);
    };
    assert!(g.module.is_none());
    assert!(matches!(&g.items[0].kind, ItemKind::Interface(i) if i.name.name == "W"));
    let ItemKind::Augment(a) = &m.items[1].kind else {
        panic!("not an augmentation: {:?}", m.items[1]);
    };
    assert_eq!(a.module.as_ref().map(|(s, _)| s.as_str()), Some("./m"));
    assert!(a.items[0].exported);
    assert_eq!(
        errors("declare global {\n  var x: number;\n}"),
        vec!["a variable cannot be declared in `declare global` or `declare module` yet: only interfaces, which merge into the declaration they augment"]
    );
    assert_eq!(
        errors("declare module \"virtual:x\";"),
        vec!["a shorthand ambient module declaration (`declare module \"…\";`) is not supported: Velt types every module from its source"]
    );
}

#[test]
fn members_end_at_a_line_break() {
    // TypeScript's automatic semicolon insertion between interface and object type members.
    let none: Vec<String> = vec![];
    assert_eq!(
        errors(
            "interface I {\n  a: number\n  readonly b: string // c\n  m(x: number): string\n  n?(): void\n  get g(): number\n}\n\
             interface J {\n  [k: string]: number\n}\n\
             type O = {\n  a: number\n  m(): void\n  [k: string]: number\n};\n\
             class C {\n  a = 1\n  b: string = \"\"\n  m(): void {}\n}"
        ),
        none
    );
    // On one line, a separator is still needed.
    assert_eq!(errors("interface I { a: number b: string }").len(), 1);
}

#[test]
fn numeric_property_names_and_call_signatures() {
    let e = expr("x = { 7: 1, 0x10: 2 }");
    let ExprKind::Assign { value, .. } = &e.kind else {
        panic!("not an assignment: {e:?}");
    };
    let ExprKind::Object(props) = &value.kind else {
        panic!("not an object literal");
    };
    assert!(matches!(&props[0], ObjectProp::KeyValue(k, _) if k.name == "7"));
    assert!(matches!(&props[1], ObjectProp::KeyValue(k, _) if k.name == "16"));
    let msg = "call signatures in an interface or object type are not supported yet";
    assert_eq!(
        errors("interface F {\n  (x: number): string\n  y: number\n}\ntype G = { <T>(x: T): T; z: string };"),
        vec![msg, msg]
    );
    // `new(…)` in an interface or object type is a construct signature, as in TypeScript; in a
    // class it is a method.
    let ctor = "construct signatures in an interface or object type are not supported yet";
    assert_eq!(
        errors("interface P {\n  new (x: number): P\n  y: number\n}\ntype Q = { new <T>(): T; z: string };"),
        vec![ctor, ctor]
    );
    let none: Vec<String> = vec![];
    assert_eq!(
        errors("interface N { new: number }\nclass C { new(): C { return this; } }"),
        none
    );
}

#[test]
fn a_cast_to_a_union_with_a_number_literal_is_an_error() {
    let msg = "a cast to a union with a number literal member is ambiguous";
    assert_eq!(errors("const y = x as i32 | 1;"), vec![msg]);
    assert_eq!(errors("const y = x as number | -0.5;"), vec![msg]);
    let none: Vec<String> = vec![];
    assert_eq!(
        errors("const y = (x as i32) | 1;\nconst z = x as (i32 | 1);"),
        none
    );
    assert_eq!(errors("const y = x as \"a\" | \"b\";"), none);
}

#[test]
fn function_expressions_are_arrows() {
    let e = expr("x = async function name<T>(a: T, b) { return a; }");
    let ExprKind::Assign { value, .. } = &e.kind else {
        panic!()
    };
    let ExprKind::Arrow {
        type_params,
        params,
        is_async,
        body: ArrowBody::Block(_),
        ..
    } = &value.kind
    else {
        panic!("expected an arrow, got {:?}", value.kind)
    };
    assert_eq!((type_params.len(), params.len(), *is_async), (1, 2, true));
    assert_eq!(
        errors("const f = function () { return this; };"),
        vec!["'this' implicitly has type 'any' because it does not have a type annotation (TypeScript's TS2683)"]
    );
    // `this` in a class declared inside the function is the class's.
    assert!(errors("const f = function () { class C { m() { return this; } } };").is_empty());
    assert_eq!(
        errors("const f = function g(n: number): number { return g(n); };"),
        vec!["a function expression's own name (`g`) inside its body is not supported yet"]
    );
    // A generator function expression's `this` is its own too.
    assert_eq!(
        errors("const g = function* () { yield this; };"),
        vec!["'this' implicitly has type 'any' because it does not have a type annotation (TypeScript's TS2683)"]
    );
    // As an object-literal property, TypeScript types `this` as the literal.
    for src in [
        "const o = { v: 1, get: function () { return this.v; } };",
        "const o = { v: 1, get: async function () { return this.v; } };",
        "const o = { v: 1, g: function* () { yield this.v; } };",
    ] {
        assert_eq!(
            errors(src),
            vec!["`this` in a function expression that is an object-literal property is not supported yet"],
            "{src}"
        );
    }
    // Only the property's own value: a function expression nested in it is not one.
    assert_eq!(
        errors("const o = { v: [1].map(function () { return this; }) };"),
        vec!["'this' implicitly has type 'any' because it does not have a type annotation (TypeScript's TS2683)"]
    );
    // After `?` of a conditional.
    parse_ok(
        "const f = b ? function (x: number) { return x; } : function (x: number) { return -x; };",
    );
    parse_ok("const f = b ? async function () {} : function* () {};");
}

#[test]
fn this_parameters_and_the_this_type() {
    let m = parse_ok(
        "function f(this: void, x: number) {} type F = (this: Window, e: number) => void;",
    );
    let ItemKind::Function(f) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(f.sig.params.len(), 1);
    assert_eq!(alias_ty(&m, 1), "fn(number) => void");
    // The AST does not keep them: tools that print it back are told where the first one is.
    let src = "const g = 1; function f(x: number) {} type F = (this: void) => void;";
    let at = velt_syntax::first_this_param(src).expect("a `this` parameter");
    assert_eq!(&src[at.start as usize..at.end as usize], "this: void");
    assert_eq!(
        velt_syntax::first_this_param("const t = this; f(this);"),
        None
    );
    assert_eq!(
        errors("function f(this: { n: number }) { return this.n; }"),
        vec!["`this` in a function with a `this` parameter is not supported yet"]
    );
    assert_eq!(
        errors("const f = (this: number) => 1;"),
        vec!["an arrow function cannot have a 'this' parameter (TypeScript's TS2730)"]
    );
    let m = parse_ok("class C { m(): this { return this; } }");
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(ty(c.methods[0].decl.sig.ret.as_ref().unwrap()), "this");
}
