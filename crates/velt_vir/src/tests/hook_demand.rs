//! Lowering calls a hook whose table entry lists constructs only where sema demanded it
//! (velt_common `hook_demand.rs`, `hir::Program::hooks_demanded`), checked in release compilers
//! too: a construct missing from the table is an ICE naming it, not a call of an unchecked stub.

use velt_sema::hir;

use super::{lower_ok, lower_src, std_src};

const PRINTS_AN_INTEGER: &str =
    "function main() {\n  const n: i64 = 7;\n  console.log(`n=${n}`);\n}\n";

#[test]
fn a_demanded_hook_lowers() {
    let v = lower_src(PRINTS_AN_INTEGER);
    assert!(
        v.funcs.iter().any(|f| f.symbol.contains("intPush")),
        "no call of intPush:\n{v}"
    );
}

#[test]
#[should_panic(
    expected = "ICE: hook int_push reached by lowering but not demanded by sema (construct "
)]
fn a_hook_sema_did_not_demand_is_an_ice() {
    let mut p = std_src::check_src(PRINTS_AN_INTEGER);
    p.hooks_demanded = Default::default();
    lower_ok(&p);
}

/// With a subclass, so that it has a table.
const CLASS_NOTHING_PRINTS: &str = "class P { v: u32 = 1; }\nclass Q extends P { w: i64 = 2; }\n\
     function main() {\n  const p: P = new Q();\n  console.log(p.v > 0);\n}\n";

/// Nothing formats it, so its table has no format glue, and the glue's hooks aren't demanded
/// (#2513).
#[test]
fn a_class_nothing_formats_has_no_format_glue() {
    let v = lower_src(CLASS_NOTHING_PRINTS);
    assert!(
        !v.funcs.iter().any(|f| f.symbol.contains("objformat")),
        "format glue:\n{v}"
    );
}

/// With it gone from the table, each path that reaches formatting must still get it. Every class
/// here has a header, so its text goes through that slot: printed, in a template, in an `any` and
/// in an interface, printed in an array and in a field, or thrown.
#[test]
fn every_way_of_formatting_a_class_builds_its_format_glue() {
    let class = "class P { v: u32 = 1; }\nclass Q extends P { w: i64 = 2; }\n";
    for main in [
        "function main() { const p: P = new Q(); console.log(p); }",
        "function main() { const p: P = new Q(); console.log(`${p}`); }",
        "function main() { const p: P = new Q(); console.log(String(p)); }",
        "function main() { const a: any = new Q(); console.log(a); }",
        "function main() { const a: unknown = new Q(); console.log(a); }",
        "function main() { const ps: P[] = [new Q()]; console.log(ps); }",
        "class R { p: P = new Q(); }\nfunction main() { console.log(new R()); }",
        "function main() { const m = new Map<string, P>(); m.set(\"a\", new Q()); console.log(m); }",
        "function main() { throw new Q(); }",
        "async function main() { throw new Q(); }",
        "function f(): void throws P { throw new Q(); }\nfunction main() { f(); }",
        // Inferred, with no `throw` and no `throws` on a function with a body.
        "interface I { f(): void throws P; }\nclass C implements I { f(): void { } }\n\
         function main() { new C().f(); }",
        "interface I { f(): void throws P; }\nclass C implements I { f(): void { } }\n\
         function g<T extends I>(t: T) { t.f(); }\nfunction main() { g(new C()); }",
        "interface I<E> { f(): void throws E; }\nclass C implements I<P> { f(): void { } }\n\
         function main() { new C().f(); }",
        // Reported where a module constant is made, with no caller to show the instance.
        "interface I<E> { f(): void throws E; }\nclass C<E> implements I<E> { f(): void { } }\n\
         const g = new C<Q>().f();\nfunction main() { }",
        // The same through a constructor, whatever expression reaches it.
        "interface I<E> { f(): void throws E; }\nclass C<E> implements I<E> { f(): void { } }\n\
         class D<E> { constructor() { new C<E>().f(); } }\nconst g = new D<Q>();\n\
         function main() { }",
        "interface I<E> { f(): void throws E; }\nclass C<E> implements I<E> { f(): void { } }\n\
         class D<E> { x: i32 = 1; constructor() { new C<E>().f(); } }\n\
         class K<E> extends D<E> { }\nconst g = new K<Q>();\nfunction main() { }",
        "interface I<E> { f(): void throws E; }\nclass C<E> implements I<E> { f(): void { } }\n\
         class D<E> { constructor(e: E) { new C<E>().f(); } }\nconst g = new D(new Q());\n\
         function main() { }",
        "interface I<E> { f(): void throws E; }\nclass C<E> implements I<E> { f(): void { } }\n\
         class D<E> { constructor() { new C<E>().f(); } }\nconst g = { a: new D<Q>() };\n\
         function main() { }",
        "interface I<E> { f(): void throws E; }\nclass C<E> implements I<E> { f(): void { } }\n\
         class D<E> { x: i32 = 1; constructor() { new C<E>().f(); } }\n\
         const g = new D<Q>().x;\nfunction main() { }",
        "interface I<E> { f(): void throws E; }\nclass C<E> implements I<E> { f(): void { } }\n\
         class D<E> { constructor() { new C<E>().f(); } }\n\
         class S { static g = new D<Q>(); }\nfunction main() { }",
        "abstract class A { abstract f(): void throws P; }\nclass C extends A { f(): void { } }\n\
         function main() { new C().f(); }",
        "interface I { f(): void; }\nclass R extends P implements I { f() { } }\n\
         function show(a: any) { console.log(a); }\n\
         function main() { const i: I = new R(); show(i); }",
    ] {
        let v = lower_src(&format!("{class}{main}\n"));
        assert!(
            (v.funcs.iter()).any(|f| f.symbol.contains("format")),
            "no format glue for\n{main}\n{v}"
        );
    }
}

/// An `Error` a function has only from what it implements declares: its report is built too.
#[test]
fn an_error_only_an_interface_declares_lowers() {
    lower_src(
        "interface I { f(): void throws RangeError; }\nclass C implements I { f(): void { } }\n\
         function main() { new C().f(); console.log(true); }\n",
    );
    lower_src(
        "class E extends Error { }\ninterface I { f(): void throws E; }\n\
         class C implements I { f(): void { } }\n\
         function main() { new C().f(); console.log(true); }\n",
    );
}

/// A function type's `throws`, with no value that throws it: inference sees the closure, so
/// nothing is reported, and the program lowers.
#[test]
fn a_thrown_type_only_a_function_type_names_lowers() {
    let class = "class P { v: u32 = 1; }\nclass Q extends P { }\n";
    lower_src(&format!(
        "{class}function call(f: () => void throws P) {{ f(); }}\n\
         function main() {{ call(() => {{}}); }}\n"
    ));
    lower_src(&format!(
        "{class}function call<E>(f: () => void throws E) {{ f(); }}\n\
         function main() {{ call<P>(() => {{}}); }}\n"
    ));
}

/// A path sema doesn't count stops the build, instead of calling a slot that holds null.
#[test]
#[should_panic(expected = "ICE: formatting P reached by lowering but not demanded by sema")]
fn formatting_a_class_sema_did_not_demand_is_an_ice() {
    let mut p = std_src::check_src(
        "class P { s: string = \"a\"; }\nclass Q extends P { }\n\
         function main() { const p: P = new Q(); console.log(p); }\n",
    );
    p.hooks_demanded = Default::default();
    lower_ok(&p);
}

/// Sema's demand must cover what lowering prints an uncaught throw with (#2312's review: without
/// it, an ICE on these one-line programs that no golden shows).
#[test]
fn an_integer_thrown_and_not_caught_lowers() {
    lower_src("function main() { throw 5 as i32; }\n");
    lower_src("async function main() { throw 5 as i32; }\n");
}

/// The signature alone makes lowering print the error, whatever the body throws (#2312's review).
#[test]
fn a_declared_integer_error_nothing_throws_lowers() {
    lower_src("function f(): void throws i32 {}\nfunction main() { f(); }\n");
    lower_src("function f(): void throws u8 {}\nfunction main() { f(); }\n");
    lower_src("function main(): void throws i32 {}\n");
}

/// Sema gives every index it builds an unsigned type, so no program reaches the out-of-bounds
/// message of a signed index (`Work::Oob(true)`, `int_text` for `IndexCheck`); lowering still
/// handles one, so this index is made signed by hand.
#[test]
fn a_signed_index_is_in_the_table() {
    let mut p = std_src::check_src(
        "function f(xs: i64[], i: i64): i64 {\n  return xs[i];\n}\n\
         function main() {\n  console.log(f([1], 0));\n}\n",
    );
    let hir::Def::Fn(f) = (p.defs.iter_mut())
        .find(|d| matches!(d, hir::Def::Fn(f) if f.name == "f"))
        .expect("f")
    else {
        unreachable!()
    };
    let signed = f.body.locals[1].ty;
    let Some(hir::StmtKind::Return(Some(e))) = f.body.block.stmts.last_mut().map(|s| &mut s.kind)
    else {
        panic!("f returns");
    };
    let hir::ExprKind::Index { index, .. } = &mut e.kind else {
        panic!("f returns an index: {e:?}");
    };
    if let hir::ExprKind::Cast(from) = &index.kind {
        *index = from.clone();
    }
    assert_eq!(index.ty, signed);
    let v = lower_ok(&p);
    assert!(
        v.funcs.iter().any(|f| f.symbol == "_Goob_i64"),
        "no signed out-of-bounds helper:\n{v}"
    );
}
