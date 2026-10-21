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
    assert!(r.contains("cannot be read like a `A[]`"), "{r}");
}

#[test]
fn a_class_with_its_own_clone_keeps_the_shared_value() {
    // Copying what a read gets would run `clone()` on objects other threads read.
    let r = err_src(
        "class C { hits: number = 0; clone(): C { this.hits++; return new C(); } }
         function count(xs: C[]): number { return xs.length; }
         function main() { const s = shared([new C()]); console.log(count(s)); }",
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

#[test]
fn typed_array_writes_through_it_are_errors() {
    for src in [
        "function main() { const s = shared(new Float64Array(3)); s[0] = 5; }",
        "function main() { const s = shared(new Float64Array(3)); s.set([1, 2], 0); }",
        "function main() { const s = shared(new Float64Array(3)); s.fill(1); }",
    ] {
        let r = err_src(src);
        assert!(r.contains("read-only"), "{src}: {r}");
    }
}

#[test]
fn a_method_returning_the_this_it_modifies_is_an_error() {
    // `bump` returns `this`, so it takes it owned: the change hides behind the move.
    let r = err_src(
        "class C { n: number = 0; bump(): C { this.n += 1; return this; } }
         function twice(c: C): C { return c.bump().bump(); }
         function main() { const s = shared(new C()); s.bump(); }",
    );
    assert!(r.contains("read out of a `shared` value"), "{r}");
    // So is one that calls such a method on `this`.
    let r = err_src(
        "class C { n: number = 0; bump(): C { this.n += 1; return this; } again(): C { return this.bump(); } }
         function main() { const s = shared(new C()); s.again(); }",
    );
    assert!(r.contains("read out of a `shared` value"), "{r}");
    // A function taking it owned gets its own copy, as `keep` in `shared_reads.vlt` does.
    ok_src(
        "function g(a: Float64Array): Float64Array { return a.fill(1); }
         function main() { const s = shared(new Float64Array(3)); g(s); }",
    );
    // Reading through such a method's sibling that leaves `this` alone is fine.
    ok_src(
        "class C { n: number = 0; bump(): C { this.n += 1; return this; } get(): number { return this.n; } }
         function main() { const s = shared(new C()); console.log(s.get()); }",
    );
}

#[test]
fn for_of_and_index_read_maps_sets_and_typed_arrays_inside() {
    let n = shared_reads(
        "function main() {
           const sm = shared(new Map<string, number>());
           const ss = shared(new Set<string>());
           const sf = shared(new Float64Array(2));
           let t = 0;
           for (const [k, v] of sm) { t += v; }
           for (const k of ss) { t += k.length; }
           for (const x of sf) { t += x; }
           console.log(t, sf[0]);
         }",
        "main",
    );
    // Each loop reads the value at least once (a typed array per length and element).
    assert!(n >= 4, "{n}");
}

#[test]
fn changing_loop_elements_of_it_is_an_error() {
    let class = "class C { n: number = 0; bump(): C { this.n += 1; return this; } inc() { this.n += 1; } }";
    for body in [
        "const s = shared(new Map<string, Uint8Array>()); for (const [k, v] of s) { v.fill(1); }",
        "const s = shared(new Set<C>()); for (const c of s) { c.bump(); }",
        "const s = shared(new Map<string, C>()); for (const [k, v] of s) { v.bump(); }",
        "const s = shared(new Map<string, C>()); for (const v of s.values()) { v.inc(); }",
        "const s = shared([new C()]); for (const c of s) { c.inc(); }",
        "const s = shared([[1], [2]]); for (const xs of s) { xs.push(3); }",
        "const s = shared([new C()]); for (const c of s.slice()) { c.n = 2; }",
    ] {
        let r = err_src(&format!("{class} function main() {{ {body} }}"));
        assert!(r.contains("the loop changes"), "{body}: {r}");
    }
    // Reading them is fine.
    ok_src(&format!(
        "{class} function main() {{ const s = shared(new Map<string, C>()); let t = 0; \
         for (const [k, v] of s) {{ t += v.n; }} for (const c of s.values()) {{ t += c.n; }} console.log(t); }}"
    ));
}

#[test]
fn views_of_a_shared_typed_array_are_errors() {
    for src in [
        "function main() { const u = shared(new Uint8Array(3)); u.subarray(0, 2).fill(3); }",
        "function main() { const u = shared(new Uint8Array(3)); const w = u.subarray(1); w[0] = 9; }",
        "function main() { const u = shared(new Uint8Array(3)); const v = new Uint8Array(u.buffer); }",
    ] {
        let r = err_src(src);
        assert!(r.contains("would share its buffer"), "{src}: {r}");
    }
    ok_src("function main() { const u = shared(new Uint8Array(3)); const c = u.slice(0, 2); c[0] = 1; console.log(c[0], u[0]); }");
}
