//! Generic recursion whose type arguments grow (`f<T>` calling `f<T[]>`) has infinitely many
//! instantiations: it is reported at the growing call instead of hanging the requirement passes
//! (JSON forms, `Record` keys, thrown types) or lowering. Recursion with the same type
//! arguments still compiles.

mod common;

use std::sync::mpsc;
use std::time::Duration;

use common::programs::{err_src, ok_src};

/// The errors of `src`, failing the test if checking takes more than a few seconds.
fn quick_errors(src: &'static str) -> String {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(err_src(src));
    });
    rx.recv_timeout(Duration::from_secs(30))
        .expect("checking a growing generic recursion did not finish")
}

#[test]
fn growing_recursion_through_json_parse_is_reported_quickly() {
    let e = quick_errors(
        "type Box<T> = { v: T };
         function f<T>(s: string, n: i64): string {
           if (n == 0) { return JSON.stringify(JSON.parse<Box<T>>(s)); }
           return f<T[]>(s, n - 1);
         }
         function main() { console.log(f<i64>(\"{}\", 0)); }",
    );
    assert!(
        e.contains("instantiating `f<T[]>` from `f<T>` grows without end"),
        "{e}"
    );
    assert!(e.contains("`f` calls itself with `T[]`"), "{e}");
}

#[test]
fn growing_recursion_through_record_keys_is_reported_quickly() {
    let e = quick_errors(
        "function f<K>(r: Record<string, K>, n: i64): i64 {
           return n == 0 ? 0 : f<Record<string, K>>({}, n - 1);
         }
         function main() { console.log(f<i64>({}, 1)); }",
    );
    assert!(
        e.contains("instantiating `f<Record<string, K>>` from `f<K>` grows without end"),
        "{e}"
    );
}

#[test]
fn mutual_growing_recursion_names_the_cycle() {
    let e = quick_errors(
        "function f<T>(x: T, n: i64): i64 { return n == 0 ? 0 : g<T[]>([x], n - 1); }
         function g<U>(x: U, n: i64): i64 { return h<U>(x, n); }
         function h<V>(x: V, n: i64): i64 { const k = (y: V): i64 => f<V>(y, n); return k(x); }
         function main() { console.log(f<i64>(1, 3)); }",
    );
    assert!(
        e.contains("instantiating `g<T[]>` from `f<T>` grows without end"),
        "{e}"
    );
    assert!(
        e.contains("`f` calls `g` with `T[]`, which calls `f` again through `h`"),
        "{e}"
    );
    assert_eq!(e.matches("grows without end").count(), 1, "{e}");
}

#[test]
fn growing_method_recursion_through_new_is_reported() {
    let e = quick_errors(
        "class Box<T> {
           v: T;
           constructor(v: T) { this.v = v; }
           deep(n: i64): i64 { return n == 0 ? 0 : new Box<T[]>([this.v]).deep(n - 1); }
         }
         function main() { console.log(new Box<i64>(1).deep(3)); }",
    );
    assert!(e.contains("grows without end"), "{e}");
}

#[test]
fn recursion_with_the_same_type_arguments_compiles() {
    ok_src(
        "type Box<T> = { v: T };
         function f<T>(s: string, n: i64): string {
           if (n == 0) { return JSON.stringify(JSON.parse<Box<T>>(s)); }
           return f<T>(s, n - 1);
         }
         function g<T>(x: T, n: i64): i64 { return n == 0 ? 0 : h<T>(x, n - 1); }
         function h<U>(x: U, n: i64): i64 { return n == 0 ? 0 : g<U>(x, n - 1); }
         function swap<A, B>(a: A, b: B, n: i64): i64 { return n == 0 ? 0 : swap<B, A>(b, a, n - 1); }
         function main() {
           console.log(f<i64>(\"{\\\"v\\\":1}\", 2), g<i64>(1, 5), swap<i64, string>(1, \"a\", 3));
         }",
    );
}

#[test]
fn a_growing_call_outside_a_cycle_compiles() {
    ok_src(
        "function leaf<T>(x: T): i64 { return 1; }
         function f<T>(x: T): i64 { return leaf<T[]>([x]) + leaf<T[][]>([[x]]); }
         function main() { console.log(f<i64>(1)); }",
    );
}

/// Field initializers constructing their own class with growing type arguments, in two ways at
/// each step: counting their errors stops along each chain and in all (#372), so checking ends.
#[test]
fn growing_field_initializers_in_two_ways_finish() {
    let src = "class E1 extends Error {}
         function call<E>(f: () => i64 throws E): i64 throws E { return f(); }
         function mkf<E>(): () => i64 throws E { return (): i64 throws E => 1; }
         function never(): bool { return false; }
         class Pair<T> { a: i64 = 0; }
         class Grow<E> {
           n: i64 = never() ? new Grow<Grow<E>>().n + new Grow<Pair<E>>().n : call<E>(mkf<E>());
         }
         function main() {
           try { console.log(new Grow<E1>().n); } catch (e) { console.log(\"caught\"); }
         }";
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(common::programs::load_src(src).check().1.len());
    });
    rx.recv_timeout(Duration::from_secs(30))
        .expect("checking growing field initializers did not finish");
}

/// The error union of initializers growing in three ways is long; the message shows its first
/// members and a count instead of hundreds of kilobytes.
#[test]
fn a_long_error_union_is_shortened_in_messages() {
    let e = quick_errors(
        "class E1 extends Error {}
         function call<E>(f: () => i64 throws E): i64 throws E { return f(); }
         function mkf<E>(): () => i64 throws E { return (): i64 throws E => 1; }
         function never(): bool { return false; }
         class Pair<T> { a: i64 = 0; }
         class Tri<T> { a: i64 = 0; }
         class Grow<E> {
           n: i64 = never()
             ? new Grow<Grow<E>>().n + new Grow<Pair<E>>().n + new Grow<Tri<E>>().n
             : call<E>(mkf<E>());
         }
         class Grow2<E> {
           n: i64 = never()
             ? new Grow2<Grow<E>>().n + new Grow2<Pair<E>>().n + new Grow<Tri<E>>().n + new Grow2<Tri<E>>().n
             : call<E>(mkf<E>());
         }
         function f1(): void {
           try { console.log(new Grow<E1>().n + new Grow2<E1>().n); } catch (e) { console.log(\"caught\"); }
         }
         function main() { f1(); }",
    );
    assert!(e.contains("the error type here is not known yet"), "{e}");
    assert!(e.contains("more)`"), "{e}");
    assert!(e.len() < 2000, "{e}");
}

/// `src` checks without errors, failing the test if checking takes more than a few seconds.
fn quick_ok(src: &'static str) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let l = common::programs::load_src(src);
        let (p, d) = l.check();
        let errors = d.iter().any(|d| d.is_error());
        let _ = tx.send(
            (p.is_some() && !errors)
                .then(String::new)
                .unwrap_or(l.render(&d)),
        );
    });
    let e = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("checking a guarded generic recursion did not finish");
    assert!(e.is_empty(), "unexpected diagnostics:\n{e}");
}

const IS_PRIMITIVE: &str = "type Primitive = string | number | boolean | null | undefined;
     function isPrimitive(v: unknown): v is Primitive {
       return v === null || (typeof v !== \"object\" && typeof v !== \"function\");
     }";

/// A growing call in the branch of a type predicate that rules out objects, with an object type
/// as the grown argument (issue #1251): the requirement passes carry what the body needs (JSON
/// forms, printed and copied values, deferred types) through it to a finite set of types.
#[test]
fn growing_recursion_behind_a_primitive_predicate_compiles() {
    let src: &'static str = Box::leak(
        format!(
            "{IS_PRIMITIVE}
             type Wrapped<T> = T extends object ? T : {{ value: T }};
             function wrap<T>(x: T): string {{
               if (isPrimitive(x)) {{
                 const inner: Wrapped<T> = {{ value: x }} as unknown as Wrapped<T>;
                 return wrap({{ value: x }}) + String(inner !== null);
               }}
               console.log(x);
               const copy = [x, x];
               return JSON.stringify(x) + copy.length;
             }}
             function main() {{ console.log(wrap(1), wrap(\"a\"), wrap({{ a: 1 }})); }}"
        )
        .into_boxed_str(),
    );
    quick_ok(src);
}

/// The guard must be on the parameter that grows, its type must exclude objects, the call must be
/// in the branch where it holds and the grown argument must be an object type.
#[test]
fn growing_recursion_the_predicate_does_not_bound_is_reported() {
    for (body, call) in [
        (
            "if (isPrimitive(x)) return f(x, { value: y });",
            "f<T, { value: U }>",
        ),
        (
            "if (isPrimitive(x)) return f<T | null, U>(null, y);",
            "f<T | null, U>",
        ),
        (
            "if (!isPrimitive(x)) return f({ value: x }, y);",
            "f<{ value: T }, U>",
        ),
        (
            "if (isPrimitive(x)) {} else return f({ value: x }, y);",
            "f<{ value: T }, U>",
        ),
        (
            "if (isSmall(x)) return f({ value: x }, y);",
            "f<{ value: T }, U>",
        ),
    ] {
        let src = format!(
            "{IS_PRIMITIVE}
             function isSmall(v: unknown): v is number | {{ n: number }} {{ return typeof v === \"number\"; }}
             function f<T, U>(x: T, y: U): number {{ {body} return 0; }}
             function main() {{ console.log(f(1, 2)); }}"
        );
        let e = quick_errors(Box::leak(src.into_boxed_str()));
        assert!(
            e.contains(&format!("instantiating `{call}`")),
            "{body}\n{e}"
        );
        assert!(e.contains("grows without end"), "{body}\n{e}");
    }
}
