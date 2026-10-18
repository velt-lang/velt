//! Optional parameters and fields are `T | undefined` (#930 phase 2): `=== null` on such a value
//! is always false and warns, `null` for an optional parameter is TS2322, and `x ?? null` keeps
//! the payload's option.

mod common;

use common::programs::{err_src, load_src, ok_src};

#[test]
fn strict_null_test_on_an_optional_parameter_warns() {
    let src = "function opt(p?: string): boolean { return p === null; }
         function opt2(p?: string): boolean { return p !== null; }
         function main() { console.log(opt(), opt2(\"x\")); }";
    let l = load_src(src);
    let (p, d) = l.check();
    let r = l.render(&d);
    assert!(p.is_some(), "{r}");
    assert!(d.iter().all(|d| !d.is_error()), "{r}");
    assert!(
        r.contains(
            "warning: this `=== null` is always false: `string | undefined` is never `null`"
        ),
        "{r}"
    );
    assert!(
        r.contains("warning: this `!== null` is always true: `string | undefined` is never `null`"),
        "{r}"
    );
    assert!(r.contains("write `== null` to test for either"), "{r}");
}

#[test]
fn loose_null_test_on_an_optional_parameter_does_not_warn() {
    let src = "function opt(p?: string): boolean { return p == null || p === undefined; }
         function main() { console.log(opt()); }";
    let l = load_src(src);
    let (p, d) = l.check();
    assert!(p.is_some() && d.is_empty(), "{}", l.render(&d));
}

#[test]
fn null_for_an_optional_parameter_is_an_error() {
    let r = err_src(
        "function greet(name: string, greeting?: string): string { return greeting ?? name; }
         function main() { console.log(greet(\"a\", null)); }",
    );
    assert!(
        r.contains("type `null` is not assignable to type `string | undefined`"),
        "{r}"
    );
}

#[test]
fn nullish_with_a_nothing_keeps_the_payload_option() {
    ok_src(
        "class Rec { state: string = \"s\"; }
         function f(r: Rec | undefined): string | null { return r?.state ?? null; }
         function g(s: string | null): string | undefined { return s ?? undefined; }
         function main() { console.log(f(undefined), g(null)); }",
    );
}
