//! `extend X implements I` (collect/ext_impls.rs): impls from `extend` blocks, the most specific
//! block per type, blanket blocks giving type parameters the interface, and the diagnostics.

mod common;

use common::hir_walk::{calls, func};
use common::programs::{err_src, load_file, ok_src};
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

#[test]
fn blocks_whose_bounds_lead_back_to_them_end_the_lookup() {
    // Each block's bound is the other's interface: a type with neither has neither (the
    // lookup that comes back to itself does not fit) instead of overflowing the stack.
    let r = err_src(
        "interface I { i(): string; }
         interface J { j(): string; }
         extend<T extends I> T implements J { j(): string { return \"j\"; } }
         extend<U extends J> U implements I { i(): string { return \"i\"; } }
         class C {}
         function f<T extends J>(x: T): string { return x.j(); }
         function main() { console.log(f(new C())); }",
    );
    assert!(r.contains("the type `C` does not implement `J`"), "{r}");
    // With an `I` of its own, a type gets `J` through the blanket block.
    ok_src(
        "interface I { i(): string; }
         interface J { j(): string; }
         extend<T extends I> T implements J { j(): string { return \"j\"; } }
         extend<U extends J> U implements I { i(): string { return \"i\"; } }
         extend u64 implements I { i(): string { return \"u\"; } }
         function f<T extends J>(x: T): string { return x.j(); }
         function main() { console.log(f(1 as u64)); }",
    );
}

#[test]
fn a_blanket_block_bounded_by_its_own_interface_is_refused() {
    let r = err_src(
        "interface I { i(): string; }
         extend<T extends I> T implements I { i(): string { return \"i\"; } }
         class C {}
         function f<T extends I>(x: T): string { return x.i(); }
         function main() { console.log(f(new C())); }",
    );
    assert!(
        r.contains("`extend T` implements `I` only for types that already implement it"),
        "{r}"
    );
    assert!(r.contains("the type `C` does not implement `I`"), "{r}");
    let r = err_src(
        "extend<T extends Comparable<T>> T implements Comparable<T> {
           compareTo(o: T): i64 { return 0; }
         }
         class C {}
         function f<T extends Comparable<T>>(x: T): i64 { return x.compareTo(x); }
         function main() { console.log(f(new C())); }",
    );
    assert!(
        r.contains("implements `Comparable` only for types that already implement it"),
        "{r}"
    );
}

#[test]
fn a_class_implementation_comes_before_blocks() {
    // `E` inherits `C`'s implementation: no conflict with the blanket block, and a block for
    // a class that already has one (its own or a base class's) is refused.
    let p = ok_src(
        "interface D { d(): string; }
         class C implements D { d(): string { return \"C\"; } }
         class E extends C {}
         extend<T> T implements D { d(): string { return \"blanket\"; } }
         function main() {}",
    );
    assert_eq!(impl_count(&p, "D"), 2);
    let r = err_src(
        "interface D { d(): string; }
         class C implements D { d(): string { return \"C\"; } }
         class E extends C {}
         extend E implements D { d(): string { return \"E\"; } }
         function main() {}",
    );
    assert!(r.contains("`E` already implements `D`"), "{r}");
    let r = err_src(
        "interface D { d(): string; }
         class Box<T> implements D { d(): string { return \"box\"; } }
         extend Box<i64> implements D { d(): string { return \"i64\"; } }
         function main() {}",
    );
    assert!(r.contains("`Box<i64>` already implements `D`"), "{r}");
}

const PAIR_FIRST: &str =
    "extend<A> Pair<A, i64> implements D { d(): string { return \"first\"; } }";
const PAIR_SECOND: &str =
    "extend<B> Pair<i64, B> implements D { d(): string { return \"second\"; } }";

#[test]
fn overlapping_blocks_where_neither_is_more_specific_are_errors() {
    // Both fit `Pair<i64, i64>`: an error in either order, not the first one.
    for (x, y) in [(PAIR_FIRST, PAIR_SECOND), (PAIR_SECOND, PAIR_FIRST)] {
        let r = err_src(&format!(
            "interface D {{ d(): string; }}
             struct Pair<A, B> {{ a: A; b: B; }}
             {x}
             {y}
             function main() {{}}"
        ));
        assert!(
            r.contains("conflicting implementations of `D`: neither block is more specific than the other (both fit `Pair<i64, i64>`)"),
            "{r}"
        );
    }
    // Disjoint targets don't overlap.
    ok_src(
        "interface D { d(): string; }
         struct Pair<A, B> { a: A; b: B; }
         extend<A> Pair<A, i64> implements D { d(): string { return \"i64\"; } }
         extend<A> Pair<A, string> implements D { d(): string { return \"string\"; } }
         function main() {}",
    );
}

#[test]
fn overlapping_blocks_in_two_modules_are_errors_whatever_the_import_order() {
    let dir = std::env::temp_dir().join(format!("velt-ext-overlap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, src: &str| std::fs::write(dir.join(name), src).unwrap();
    write(
        "d.vlt",
        "export interface D { d(): string; }
         export struct Pair<A, B> { a: A; b: B; }",
    );
    for (name, block) in [("first.vlt", PAIR_FIRST), ("second.vlt", PAIR_SECOND)] {
        write(
            name,
            &format!("import {{ D, Pair }} from \"./d\"; {block} export const x = 1;"),
        );
    }
    for order in [["first", "second"], ["second", "first"]] {
        let main = dir.join(format!("main_{}.vlt", order[0]));
        std::fs::write(
            &main,
            format!(
                "import {{ x }} from \"./{}\";
                 import {{ x as y }} from \"./{}\";
                 function main() {{ console.log(x, y); }}",
                order[0], order[1]
            ),
        )
        .unwrap();
        let l = load_file(&main);
        let (p, d) = l.check();
        let r = l.render(&d);
        assert!(p.is_none(), "{r}");
        assert!(r.contains("conflicting implementations of `D`"), "{r}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dynamic_never_union_and_self_targets_are_refused() {
    for target in ["any", "unknown", "never"] {
        let r = err_src(&format!(
            "interface D {{ d(): string; }}
             extend {target} implements D {{ d(): string {{ return \"x\"; }} }}
             function main() {{}}"
        ));
        assert!(
            r.contains(&format!("`extend {target}` cannot implement `D`")),
            "{r}"
        );
    }
    let r = err_src(
        "interface D { d(): string; }
         interface D2 extends D { e(): string; }
         extend D implements D { d(): string { return \"x\"; } }
         extend D2 implements D { d(): string { return \"x\"; } }
         function main() {}",
    );
    assert!(r.contains("`D` already implements `D`"), "{r}");
    assert!(r.contains("`D2` already implements `D`"), "{r}");
    let r = err_src(
        "interface D { d(): string; }
         type U = { kind: \"a\"; x: i64 } | { kind: \"b\"; y: string };
         extend U implements D { d(): string { return this.kind; } }
         type N = string | null;
         extend N implements D { d(): string { return \"n\"; } }
         function main() {}",
    );
    assert!(
        r.contains("`extend U` cannot implement `D`: `U` is a union"),
        "{r}"
    );
    assert!(r.contains("is a union, and a value of it has the type of one of its members"));
    assert_eq!(r.matches("is a union").count(), 2, "{r}");
}

#[test]
fn an_optional_method_is_named_as_such() {
    let r = err_src(
        "interface O { o?(): string; p(): string; }
         extend i64 implements O { p(): string { return \"p\"; } }
         function main() {}",
    );
    assert!(
        r.contains("`O` declares optional method `o`, which is a field, and an `extend` block cannot add fields"),
        "{r}"
    );
    let r = err_src(
        "interface O { size?: i64; p(): string; }
         extend i64 implements O { p(): string { return \"p\"; } }
         function main() {}",
    );
    assert!(r.contains("`O` declares optional field `size`"), "{r}");
}

#[test]
fn interface_arguments_do_not_make_blocks_disjoint() {
    // A type implements an interface once: two blocks for `W` with different arguments
    // conflict, and the message shows the arguments.
    let r = err_src(
        "interface G<X> { g(): string; }
         class W {}
         extend W implements G<i64> { g(): string { return \"i64\"; } }
         extend W implements G<string> { g(): string { return \"string\"; } }
         function main() {}",
    );
    assert!(
        r.contains("conflicting implementations of `G<string>` and `G<i64>`"),
        "{r}"
    );
    // The more specific block decides the arguments: `Box<string>` is a `G<string>`.
    let r = err_src(
        "interface G<X> { g(): string; }
         class Box<T> { constructor(public v: T) {} }
         extend<T> Box<T> implements G<T> { g(): string { return \"box\"; } }
         extend<A> A implements G<i64> { g(): string { return \"any\"; } }
         function a<T extends G<i64>>(x: T): string { return x.g(); }
         function main() { console.log(a(new Box(1 as i64)), a(new Box(\"s\"))); }",
    );
    assert!(
        r.contains("`Box<string>` implements `G<string>`, not `G<i64>`"),
        "{r}"
    );
    assert_eq!(r.matches("error:").count(), 1, "{r}");
}

#[test]
fn dynamic_bounds_on_implementing_blocks_are_refused() {
    for b in ["unknown", "object", "Function", "any"] {
        let r = err_src(&format!(
            "interface D {{ d(): string; }}
             class Box<T> {{ constructor(public v: T) {{}} }}
             extend<T extends {b}> Box<T> implements D {{ d(): string {{ return \"x\"; }} }}
             extend<T extends {b}> T implements D {{ d(): string {{ return \"y\"; }} }}
             function main() {{}}"
        ));
        assert_eq!(
            r.matches(&format!("cannot bound `T` by `{b}`")).count(),
            2,
            "{r}"
        );
    }
}

/// `extend` chains: `n` levels of interfaces `A<k>`/`B<k>`, each block bounded by both
/// interfaces of the next level, and a class implementing the last.
fn ab_chain(n: usize) -> String {
    let mut s = String::new();
    for k in 0..=n {
        s +=
            &format!("interface A{k} {{ a{k}(): string; }} interface B{k} {{ b{k}(): string; }}\n");
    }
    for k in 0..n {
        let j = k + 1;
        s += &format!(
            "extend<T extends A{j} & B{j}> T implements A{k} {{ a{k}(): string {{ return \"a\"; }} }}
             extend<T extends A{j} & B{j}> T implements B{k} {{ b{k}(): string {{ return \"b\"; }} }}\n"
        );
    }
    s + &format!("class C implements A{n}, B{n} {{ a{n}(): string {{ return \"a\"; }} b{n}(): string {{ return \"b\"; }} }}\n")
}

#[test]
fn a_deep_chain_of_blocks_is_looked_up_once_per_type() {
    // Each level checks both bounds of the next: without remembering finished lookups this
    // takes 2^60 steps.
    ok_src(&format!(
        "{} function f<T extends A0>(x: T): string {{ return x.a0(); }}
         function main() {{ console.log(f(new C())); }}",
        ab_chain(60)
    ));
}

#[test]
fn blanket_blocks_chain_for_type_parameters() {
    ok_src(
        "interface K { k(): string; }
         interface J { j(): string; }
         interface I { i(): string; }
         extend<T extends K> T implements J { j(): string { return \"j\"; } }
         extend<T extends J> T implements I { i(): string { return \"i\" + this.j(); } }
         class C implements K { k(): string { return \"k\"; } }
         function needI<T extends I>(x: T): string { return x.i(); }
         function fromK<T extends K>(x: T): string { return needI(x) + x.i(); }
         function main() { console.log(needI(new C()), fromK(new C())); }",
    );
    // Ten levels, from a generic function bounded by the last one.
    let n = 10;
    let mut s = String::new();
    for k in 0..=n {
        s += &format!("interface I{k} {{ i{k}(): string; }}\n");
    }
    for k in 0..n {
        let j = k + 1;
        s += &format!(
            "extend<T extends I{j}> T implements I{k} {{ i{k}(): string {{ return \"{k}\"; }} }}\n"
        );
    }
    ok_src(&format!(
        "{s} function f<T extends I0>(x: T): string {{ return x.i0(); }}
         function g<T extends I{n}>(x: T): string {{ return f(x); }}
         function main() {{}}"
    ));
}

#[test]
fn a_long_chain_has_no_nesting_limit() {
    // Bound checks through blocks are one fixed-point computation, not nested calls.
    let n = 200;
    let mut s = String::new();
    for k in 0..=n {
        s += &format!("interface I{k} {{ i{k}(): string; }}\n");
    }
    for k in 0..n {
        let j = k + 1;
        s += &format!(
            "extend<T extends I{j}> T implements I{k} {{ i{k}(): string {{ return \"{k}\"; }} }}\n"
        );
    }
    ok_src(&format!(
        "{s} class C implements I{n} {{ i{n}(): string {{ return \"c\"; }} }}
         function f<T extends I0>(x: T): string {{ return x.i0(); }}
         function g<T extends I{n}>(x: T): string {{ return f(x); }}
         function main() {{ console.log(f(new C())); }}"
    ));
}

#[test]
fn bounds_hold_whatever_is_looked_up_first() {
    // P1 needs Q, Q1 needs P, P2 gives every type P: `C` gets the bounded P1 in both orders.
    let src = |calls: &str| {
        format!(
            "interface P {{ p(): string; }}
             interface Q {{ q(): string; }}
             extend<T extends Q> T implements P {{ p(): string {{ return \"P1\"; }} }}
             extend<T> T implements P {{ p(): string {{ return \"P2\"; }} }}
             extend<T extends P> T implements Q {{ q(): string {{ return \"Q1\"; }} }}
             class C {{}}
             function sp<T extends P>(x: T): string {{ return x.p(); }}
             function sq<T extends Q>(x: T): string {{ return x.q(); }}
             function main() {{ {calls} }}"
        )
    };
    for calls in ["sq(new C()); sp(new C());", "sp(new C()); sq(new C());"] {
        let p = ok_src(&src(calls));
        // Both orders check (goldens `lang/extend_implements_cycle_order_a/_b` run them).
        let mut bounded: Vec<usize> = p.extension_impls.values().map(|b| b.len()).collect();
        bounded.sort_unstable();
        assert_eq!(bounded, vec![0, 1, 1]);
    }
}
