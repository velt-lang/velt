//! Setters: `x.name = v` calls `set name(v)`; getter/setter pairs share a name; compound
//! assignment and `++` read through the getter, then write through the setter.

mod common;

use common::hir_walk::{calls, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Callee, Def, PassMode};

const BOX: &str = "class Box { n: i64 = 0;
  get size(): i64 { return this.n; }
  set size(v: i64) { this.n = v; } }
function make(): Box { return new Box(); }";

fn called(p: &velt_sema::hir::Program, f: &str) -> Vec<String> {
    calls(func(p, f))
        .into_iter()
        .filter_map(|(c, _)| match c {
            Callee::Def(d, _) => match p.def(*d) {
                Def::Fn(f) => Some(f.name.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn assignment_calls_the_setter_which_modifies_this() {
    let p = ok_src(&format!(
        "{BOX} function main() {{ const b = new Box(); b.size = 3; b.size += 2; b.size++; \
         make().size = 1; console.log(b.size); }}"
    ));
    let names = called(&p, "main");
    assert_eq!(
        names
            .iter()
            .filter(|n| n.as_str() == "Box.set size")
            .count(),
        4,
        "{names:?}"
    );
    assert!(names.iter().any(|n| n == "Box.size"), "{names:?}");
    let setter = func(&p, "Box.set size");
    assert_eq!(setter.params[0].mode, PassMode::BorrowMut);
}

#[test]
fn setters_through_interfaces_overrides_and_extend() {
    ok_src(
        "interface Sized { get size(): i64; set size(v: i64); }
         class A implements Sized { n: i64 = 0; get size(): i64 { return this.n; }
           set size(v: i64) { this.n = v; } }
         class B extends A { override set size(v: i64) { this.n = v * 2; } }
         struct P { x: i64; }
         extend P { get double(): i64 { return this.x * 2; } set double(v: i64) { this.x = v / 2; } }
         function grow<T extends Sized>(x: T) { x.size += 1; x.size = x.size * 2; }
         function main() { const a: A = new B(); a.size = 4; grow(a);
           let p = P { x: 1 }; p.double = 10; console.log(a.size, p.x); }",
    );
}

#[test]
fn read_modify_write_evaluates_the_receiver_once() {
    // The getter, then the setter, on one evaluation of `make()`; as values too.
    let p = ok_src(&format!(
        "{BOX} function main() {{ const b = new Box(); make().size += 1; make().size++; const x = b.size++; const y = --b.size; const z = (b.size *= 2); console.log(x, y, z); }}"
    ));
    let names = called(&p, "main");
    let count = |n: &str| names.iter().filter(|m| m.as_str() == n).count();
    assert_eq!(count("make"), 2, "{names:?}");
    assert_eq!(count("Box.set size"), 5, "{names:?}");
    assert_eq!(count("Box.size"), 5, "{names:?}");
}

#[test]
fn a_getter_in_the_receiver_runs_once() {
    // `h.inner.size += 1` reads `h.inner` once, then the getter and setter of `size` on it.
    let p = ok_src(&format!(
        "{BOX} class H {{ b: Box = new Box(); get inner(): Box {{ return this.b; }} }} function main() {{ const h = new H(); h.inner.size += 1; h.inner.size++; }}"
    ));
    let names = called(&p, "main");
    let count = |n: &str| names.iter().filter(|m| m.as_str() == n).count();
    assert_eq!(count("H.inner"), 2, "{names:?}");
    assert_eq!(count("Box.size"), 2, "{names:?}");
    assert_eq!(count("Box.set size"), 2, "{names:?}");
}

#[test]
fn setter_errors() {
    let r = err_src(
        "class S { t: string = \"\"; get s(): string { return this.t; } set s(v: string) { this.t = v; } } function main() { const x = new S(); x.s++; }",
    );
    assert!(r.contains("cannot apply `++` to type `string`"), "{r}");
    let r = err_src(
        "class S { t: string | null = null; get s(): string | null { return this.t; } set s(v: string | null) { this.t = v; } } function main() { const x = new S(); x.s ||= \"d\"; }",
    );
    assert!(
        r.contains("`||=` needs a `boolean` or nullable left side, found `string | null`"),
        "{r}"
    );
    // As a value, `x.n ??= 1` is non-null (TypeScript's type).
    ok_src(
        "class N { m: i64 | null = null; get n(): i64 | null { return this.m; } set n(v: i64 | null) { this.m = v; } } function main() { const x = new N(); const v: i64 = (x.n ??= 1); }",
    );
    let r =
        err_src("class W { set w(v: i64) {} } function main() { const x = new W(); x.w += 1; }");
    assert!(
        r.contains("cannot read `w`: it has a setter but no getter"),
        "{r}"
    );
    // Assigning through a setter modifies the param: it is inferred mutably borrowed.
    let p = ok_src(&format!(
        "{BOX} function f(b: Box) {{ b.size = 1; }} function main() {{ f(new Box()); }}"
    ));
    assert_eq!(func(&p, "f").params[0].mode, PassMode::BorrowMut);
    let r = err_src("class C { n: i64 = 0; set n(v: i64) {} } function main() {}");
    assert!(r.contains("`n` is both a field and a method"), "{r}");
    let r = err_src("class C { set n(v: i64) {} set n(v: i64) {} } function main() {}");
    assert!(r.contains("duplicate setter `n`"), "{r}");
    let r = err_src(
        "class A { set n(v: i64) {} } class B extends A { set n(v: i64) {} } function main() {}",
    );
    assert!(r.contains("redefines a base class method"), "{r}");
}
