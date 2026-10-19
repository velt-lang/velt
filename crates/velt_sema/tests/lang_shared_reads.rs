//! A `shared<T>` without a `Mutex` read like the `T` it holds (#36): index, `length`, `T`'s
//! methods and arguments where a `T` is expected go through `Intrinsic::SharedRead`; writes
//! through it are errors.

mod common;

use common::hir_walk::{calls, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Callee, Intrinsic};

fn shared_reads(src: &str, f: &str) -> usize {
    let p = ok_src(src);
    calls(func(&p, f))
        .into_iter()
        .filter(|(c, _)| matches!(c, Callee::Intrinsic(Intrinsic::SharedRead)))
        .count()
}

#[test]
fn index_length_and_arguments_read_the_value_inside() {
    let n = shared_reads(
        "function sum(xs: u8[]): i64 { let t: i64 = 0; for (const x of xs) { t += x as i64; } return t; }
         function main() {
           const s = shared([1 as u8, 2, 3]);
           console.log(s[0], s.length, sum(s));
         }",
        "main",
    );
    assert_eq!(n, 3);
}

#[test]
fn methods_of_the_value_inside() {
    let n = shared_reads(
        "function main() {
           const s = shared(\"hello\");
           console.log(s.startsWith(\"he\"), s.indexOf(\"l\"), s.length, s[1]);
         }",
        "main",
    );
    assert_eq!(n, 4);
}

#[test]
fn clone_and_atomics_stay_on_the_shared_value() {
    let n = shared_reads(
        "function main() {
           const s = shared([1, 2]);
           const t = s.clone();
           const c = shared(0 as i64);
           c.add(1);
           console.log(c.get(), t.length);
         }",
        "main",
    );
    assert_eq!(n, 1);
    // A `shared<int>` changes: it is never read as a plain value.
    let r = err_src(
        "function f(n: i64): i64 { return n; } function main() { const c = shared(0 as i64); f(c); }",
    );
    assert!(
        r.contains("mismatched types") || r.contains("expected"),
        "{r}"
    );
}

#[test]
fn writes_through_it_are_errors() {
    let r = err_src("function main() { const s = shared([1, 2]); s[0] = 5; }");
    assert!(r.contains("a shared value is read-only"), "{r}");
    let r = err_src("function main() { const s = shared([1, 2]); s.push(3); }");
    assert!(r.contains("a shared value is read-only"), "{r}");
    let r = err_src(
        "function bump(xs: number[]) { xs[0] += 1; } function main() { const s = shared([1, 2]); bump(s); }",
    );
    assert!(r.contains("read out of a `shared` value"), "{r}");
    // A function value may modify its argument.
    let r = err_src(
        "function main() { const f = (xs: number[]) => xs.length; const s = shared([1, 2]); console.log(f(s)); }",
    );
    assert!(r.contains("to a function value"), "{r}");
}

#[test]
fn a_mutex_inside_keeps_the_shared_value() {
    let r = err_src(
        "function main() { const s = shared(new Mutex<number[]>([1])); console.log(s.length); }",
    );
    assert!(r.contains("no field `length`"), "{r}");
}

#[test]
fn what_the_compiler_cannot_see_into_keeps_the_shared_value() {
    // A type parameter may be instantiated with a type that holds a `Mutex`.
    let r = err_src(
        "function first<T>(s: shared<T[]>): T { return s[0]; } function main() { console.log(first(shared([1]))); }",
    );
    assert!(r.contains("cannot index"), "{r}");
    // A class with subclasses: an object may have fields the base class doesn't declare.
    let r = err_src(
        "class A { n: number = 1; } class B extends A { m = new Mutex<number>(0); }
         function count(xs: A[]): number { return xs.length; }
         function main() { const s = shared([new A()]); console.log(count(s)); }",
    );
    assert!(r.contains("mismatched types"), "{r}");
}

#[test]
fn fields_and_for_of_read_the_value_inside() {
    let n = shared_reads(
        "struct Cfg { name: string; ports: number[]; }
         function main() {
           const s = shared<Cfg>({ name: \"x\", ports: [1, 2] });
           let t = 0;
           for (const p of s.ports) { t += p; }
           for (const c of shared(\"ab\")) { console.log(c); }
           console.log(s.name, t);
         }",
        "main",
    );
    assert_eq!(n, 3);
    let r = err_src(
        "struct Cfg { ports: number[]; } function main() { const s = shared<Cfg>({ ports: [1] }); s.ports.push(2); }",
    );
    assert!(r.contains("a shared value is read-only"), "{r}");
}
