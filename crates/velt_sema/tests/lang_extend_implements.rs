//! `extend X implements I` (collect/ext_impls.rs): impls from `extend` blocks, the most specific
//! block per type, blanket blocks giving type parameters the interface, and the diagnostics.

mod common;

use common::hir_walk::{calls, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Callee, Def};

const DESCRIBE: &str = "interface Describe { describe(): string; }
     extend<T extends Comparable<T>> T implements Describe { describe(): string { return \"any\"; } }
     extend u64 implements Describe { describe(): string { return \"u64\"; } }";

fn impl_count(p: &velt_sema::hir::Program, iface: &str) -> usize {
    p.impls
        .iter()
        .filter(|i| matches!(p.def(i.iface), Def::Interface(d) if d.name.ends_with(iface)))
        .count()
}

#[test]
fn blocks_become_impls_with_their_bounds() {
    let p = ok_src(&format!("{DESCRIBE} function main() {{}}"));
    assert_eq!(impl_count(&p, "Describe"), 2);
    // The blanket impl carries `T extends Comparable<T>`; the one for `u64` has no bounds.
    let mut bounds: Vec<usize> = p.extension_impls.values().map(|b| b.len()).collect();
    bounds.sort_unstable();
    assert_eq!(bounds, vec![0, 1]);
}

#[test]
fn a_call_on_a_type_parameter_goes_through_the_interface() {
    // `K` declares only `Comparable`; the blanket block gives it `Describe`, so the call is
    // resolved per instantiation (`ParamMethod`), not bound to the blanket block's method.
    let p = ok_src(&format!(
        "{DESCRIBE}
         function label<K extends Comparable<K>>(k: K): string {{ return k.describe(); }}
         function main() {{ console.log(label(1 as u64), label(\"a\")); }}"
    ));
    let f = func(&p, "label");
    let through_iface = calls(f).into_iter().any(|(c, _)| {
        matches!(c, Callee::ParamMethod { iface, .. }
            if matches!(p.def(*iface), Def::Interface(d) if d.name.ends_with("Describe")))
    });
    assert!(through_iface);
}

#[test]
fn a_type_parameter_meets_the_bound_through_a_blanket_block() {
    ok_src(&format!(
        "{DESCRIBE}
         function show<T extends Describe>(x: T): string {{ return x.describe(); }}
         function outer<K extends Comparable<K>>(k: K): string {{ return show(k); }}
         function main() {{ console.log(outer(true)); }}"
    ));
    // Without `Comparable`, the blanket block does not apply.
    let r = err_src(&format!(
        "{DESCRIBE}
         function show<T extends Describe>(x: T): string {{ return x.describe(); }}
         function outer<K>(k: K): string {{ return show(k); }}
         function main() {{}}"
    ));
    assert!(r.contains("does not implement `Describe`"), "{r}");
}

#[test]
fn a_type_without_the_blanket_bounds_does_not_implement_it() {
    let r = err_src(&format!(
        "{DESCRIBE}
         class C {{}}
         function show<T extends Describe>(x: T): string {{ return x.describe(); }}
         function main() {{ show(new C()); }}"
    ));
    assert!(
        r.contains("the type `C` does not implement `Describe`"),
        "{r}"
    );
}

#[test]
fn missing_and_mismatched_methods_are_errors() {
    let r = err_src(
        "interface N { name(): string; greet(): string; }
         extend i64 implements N { name(): string { return \"n\"; } }
         function main() {}",
    );
    assert!(
        r.contains("`extend i64` is missing method `greet` required by `N`"),
        "{r}"
    );
    let r = err_src(
        "interface N { name(): string; }
         extend i64 implements N { name(): i64 { return 1; } }
         function main() {}",
    );
    assert!(
        r.contains("method `name` has a different signature than required by `N`"),
        "{r}"
    );
    let r = err_src(
        "interface N { name(): string; }
         extend i64 implements N { static name(): string { return \"s\"; } }
         function main() {}",
    );
    assert!(
        r.contains("a static method does not implement an interface method"),
        "{r}"
    );
}

#[test]
fn defaults_fill_left_out_methods() {
    let p = ok_src(
        "interface N { name(): string; loud(): string { return this.name() + \"!\"; } }
         extend i64 implements N { name(): string { return \"n\"; } }
         function f<T extends N>(x: T): string { return x.loud(); }
         function main() { console.log(f(1 as i64)); }",
    );
    assert_eq!(impl_count(&p, "N"), 1);
}

#[test]
fn equally_specific_blocks_and_repeated_impls_are_errors() {
    let r = err_src(
        "interface N { name(): string; }
         extend<T extends Comparable<T>> T implements N { name(): string { return \"a\"; } }
         extend<U extends Comparable<U>> U implements N { name(): string { return \"b\"; } }
         function main() {}",
    );
    assert!(r.contains("conflicting implementations of `N`"), "{r}");
    let r = err_src(
        "interface N { name(): string; }
         class P implements N { name(): string { return \"p\"; } }
         extend P implements N { name(): string { return \"q\"; } }
         function main() {}",
    );
    assert!(r.contains("`P` already implements `N`"), "{r}");
    // A bounded and an unbounded block for one target are not equally specific.
    ok_src(
        "interface N { name(): string; }
         extend<T> Array<T> implements N { name(): string { return \"a\"; } }
         extend<T extends Comparable<T>> Array<T> implements N { name(): string { return \"b\"; } }
         function main() {}",
    );
}

#[test]
fn interfaces_with_fields_and_non_interfaces_are_refused() {
    let r = err_src(
        "interface S { size: i64; }
         extend string implements S {}
         function main() {}",
    );
    assert!(
        r.contains("`extend string` cannot implement `S`: `S` declares fields"),
        "{r}"
    );
    let r = err_src("class C {} extend i64 implements C {} function main() {}");
    assert!(r.contains("`C` is not an interface"), "{r}");
}
