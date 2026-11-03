//! `unsafe` code (docs/internals/design/c-ffi.md): calling an unsafe function needs an
//! `unsafe` block or an unsafe function around the call; unsafe contexts are lexical.

mod common;

use common::programs::{err_src, ok_src};

const RAW: &str = "unsafe function raw(n: i64): i64 { return n + 1; }
class Buf { unsafe at(i: i64): i64 { return i; } static unsafe make(): Buf { return new Buf(); } }";

#[test]
fn calls_inside_unsafe_code_are_accepted() {
    ok_src(&format!(
        "{RAW}
         unsafe function twice(n: i64): i64 {{ const g = (m: i64) => raw(m); return raw(g(n)); }}
         function main() {{
           unsafe {{ console.log(raw(1)); }}
           const b = unsafe {{ Buf.make() }};
           const x = unsafe {{ b.at(2) + twice(3) }};
           const f = (n: i64) => unsafe {{ raw(n) }};
           unsafe {{ const g = (n: i64) => raw(n); console.log(g(4)); }}
           console.log(x, f(5));
         }}"
    ));
}

#[test]
fn a_call_outside_unsafe_code_is_an_error() {
    let e = err_src(&format!("{RAW} function main() {{ console.log(raw(1)); }}"));
    assert!(
        e.contains("calling unsafe function `raw` needs an `unsafe` block or function"),
        "{e}"
    );
    assert!(e.contains("// SAFETY:"), "{e}");
}

#[test]
fn methods_and_static_methods_need_unsafe_too() {
    let e = err_src(&format!(
        "{RAW} function main() {{ const b = unsafe {{ Buf.make() }}; console.log(b.at(1)); }}"
    ));
    assert!(e.contains("calling unsafe method `at`"), "{e}");
    let e = err_src(&format!(
        "{RAW} function main() {{ const b = Buf.make(); }}"
    ));
    assert!(e.contains("calling unsafe `Buf.make`"), "{e}");
}

#[test]
fn a_nested_function_declaration_is_not_unsafe_code() {
    let e = err_src(&format!(
        "{RAW} unsafe function outer(): i64 {{ function inner(): i64 {{ return raw(1); }} return inner(); }}"
    ));
    assert!(e.contains("calling unsafe function `raw`"), "{e}");
}

#[test]
fn the_unsafe_context_ends_with_its_block() {
    let e = err_src(&format!(
        "{RAW} function main() {{ unsafe {{ console.log(raw(1)); }} console.log(raw(2)); }}"
    ));
    assert_eq!(e.matches("calling unsafe function `raw`").count(), 1, "{e}");
}

#[test]
fn an_unsafe_function_is_not_a_value() {
    let e = err_src(&format!(
        "{RAW} function main() {{ const f = raw; console.log(unsafe {{ f(1) }}); }}"
    ));
    assert!(
        e.contains("unsafe function `raw` can only be called, not used as a value"),
        "{e}"
    );
}

#[test]
fn an_unsafe_block_that_returns_diverges() {
    ok_src(&format!(
        "{RAW} function pick(n: i64): i64 {{ if (n > 0) {{ unsafe {{ return raw(n); }} }} unsafe {{ return raw(0); }} }}
         function main() {{ console.log(pick(1)); }}"
    ));
}

#[test]
fn an_unsafe_method_cant_implement_an_interface_method() {
    let e = err_src(
        "interface Op { run(): void }
         class Raw implements Op { unsafe run(): void {} }
         function main() { const o: Op = new Raw(); o.run(); }",
    );
    assert!(
        e.contains(
            "an unsafe method can't stand in for `Op.run`: interface methods are never unsafe"
        ),
        "{e}"
    );
}

#[test]
fn an_unsafe_method_cant_override_a_safe_one() {
    let e = err_src(
        "class A { m(): void {} }
         class B extends A { override unsafe m(): void {} }
         function main() { const a: A = new B(); a.m(); }",
    );
    assert!(
        e.contains("an unsafe method can't stand in for `A.m`"),
        "{e}"
    );
    // Unsafe over unsafe, and safe over unsafe, are fine.
    ok_src(
        "class A { unsafe m(): void {} }
         class B extends A { override unsafe m(): void {} }
         class C extends A { override m(): void {} }
         function main() { const a: A = new B(); unsafe { a.m(); } new C().m(); }",
    );
}

#[test]
fn local_generic_and_recursive_arrows_in_unsafe_code_stay_unsafe_code() {
    ok_src(&format!(
        "{RAW}
         function main() {{
           unsafe {{
             const id = <T>(x: T): T => {{ raw(1); return x; }};
             console.log(id(2));
           }}
         }}
         unsafe function count(n: i64): i64 {{
           const down = (k: i64): i64 => k <= 0 ? raw(0) : down(k - 1);
           return down(n);
         }}"
    ));
    let e = err_src(&format!(
        "{RAW} function main() {{ const id = <T>(x: T): T => {{ raw(1); return x; }}; console.log(id(2)); }}"
    ));
    assert!(e.contains("calling unsafe function `raw`"), "{e}");
}

#[test]
fn a_generic_unsafe_function_is_not_a_value_either() {
    let e = err_src(
        "unsafe function first<T>(xs: T[]): T { return xs[0]; }
         function main() { const f: (xs: i64[]) => i64 = first; console.log(f([1])); }",
    );
    assert!(
        e.contains("unsafe function `first` can only be called, not used as a value"),
        "{e}"
    );
}
