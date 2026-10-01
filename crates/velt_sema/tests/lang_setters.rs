//! Setters: `x.name = v` calls `set name(v)`; getter/setter pairs share a name; compound
//! assignment and `++` read through the getter and write through the setter.

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
fn setter_errors() {
    let r = err_src(&format!(
        "{BOX} function main() {{ const b = new Box(); const x = b.size++; }}"
    ));
    assert!(
        r.contains("`++` on the setter `size` cannot be used as a value"),
        "{r}"
    );
    let r = err_src(&format!("{BOX} function main() {{ make().size += 1; }}"));
    assert!(
        r.contains(
            "compound assignment through the setter `size` needs a variable or field receiver"
        ),
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
