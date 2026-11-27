//! Which interface methods a program can run without a direct call, for the checks that follow a
//! type's methods (velt_sema `dispatch::Converted`, `json`): a method of an implementation runs
//! dynamically only on a value converted to that interface, through a type parameter's bound
//! where the parameter is instantiated, or by name on a value converted to `any`. Each test
//! takes the identity-key rule of `object_copies` as the observer: a value copied into `A` is an
//! error when the program can hash an `A` (a `Set<A>` element), and only then.

mod common;

use common::programs::{err_src, ok_src};

const KEYS: &str = "this program uses `A` values as keys compared by identity";

/// `A`, a copy of an `AB` as an `A`, and a class that can hash an `A` only in its `Has` method.
const PRELUDE: &str = "type A = { a: number };\n\
     type AB = { a: number; b: number };\n\
     interface Has<T> { has(x: T): boolean }\n\
     class Bag<T> implements Has<T> {\n\
       items = new Set<T>();\n\
       has(x: T): boolean { return this.items.has(x); }\n\
     }\n\
     const s: AB = JSON.parse('{\"a\":1,\"b\":2}');\n\
     const v: A = s;\n";

fn program(rest: &str) -> String {
    format!("{PRELUDE}{rest}")
}

/// How many times `src` reports the identity-key error.
fn key_errors(src: &str) -> usize {
    err_src(src).matches(KEYS).count()
}

#[test]
fn an_implementation_nothing_converts_or_calls_is_not_reached() {
    // `Bag<A>` is made and its fields read, but no `Has<A>` value exists and nothing calls
    // `has`: no `A` is hashed, so the copy cannot be told from the original.
    ok_src(&program(
        "const b = new Bag<A>();\nconsole.log(b.items.size, v.a);",
    ));
}

#[test]
fn a_direct_call_is_reached() {
    assert_eq!(key_errors(&program("console.log(new Bag<A>().has(v));")), 1);
}

#[test]
fn a_call_through_an_interface_value_is_reached() {
    let src = program("const h: Has<A> = new Bag<A>();\nconsole.log(h.has(v));");
    assert_eq!(key_errors(&src), 1);
}

#[test]
fn a_call_through_a_bound_is_reached_where_the_parameter_is_instantiated() {
    let src = program(
        "function look<O extends Has<A>>(o: O, x: A): boolean { return o.has(x); }\n\
         console.log(look(new Bag<A>(), v));",
    );
    assert_eq!(key_errors(&src), 1);
}

#[test]
fn a_bound_call_in_a_generic_caller_is_reached_through_its_callers() {
    let src = program(
        "function look<T, O extends Has<T>>(o: O, x: T): boolean { return o.has(x); }\n\
         function twice<T>(b: Bag<T>, x: T): boolean { return look(b, x) && look(b, x); }\n\
         console.log(twice(new Bag<A>(), v));",
    );
    assert_eq!(key_errors(&src), 1);
}

#[test]
fn a_method_called_by_name_on_any_is_reached() {
    let src = program("const b: any = new Bag<A>();\nconst found: boolean = b.has(v) as boolean;\nconsole.log(found);");
    assert_eq!(key_errors(&src), 1);
}

#[test]
fn a_value_made_dynamic_in_generic_code_is_reached_where_it_is_instantiated() {
    let src = program(
        "function dyn<T>(x: T): any { return x; }\n\
         const found: boolean = dyn(new Bag<A>()).has(v) as boolean;\n         console.log(found);",
    );
    assert_eq!(key_errors(&src), 1);
}
