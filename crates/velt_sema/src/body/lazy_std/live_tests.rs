//! Each construct of `hook_demand::Construct` is found where live code has it. Once one integer
//! hook is live its std code brings the others, so what a program demands can't tell these apart
//! (#2312's review). The constructs of a program that demands one hook are those of the hook's
//! code too, so each row looks at what was found before the first hook was demanded.

use std::cell::{Cell, RefCell};

use velt_common::std_hooks::{Construct, ConstructSet};

use crate::script_locals::sema_tests::programs;
use crate::{CheckOptions, SEMA_STACK_BUDGET, SEMA_STACK_BYTES};

thread_local! {
    /// What the live set held when the last check on this thread asked for the demand set: its
    /// constructs and how many implementations it opened.
    pub(super) static PRESENT: Cell<(ConstructSet, usize)> = const { Cell::new((0, 0)) };
    /// The constructs found before the first hook was demanded in the last check on this thread.
    static BEFORE_HOOKS: Cell<ConstructSet> = const { Cell::new(0) };
    /// Have [`finish`] demand this std module, as no program does.
    static LATE_DEMAND: Cell<Option<&'static str>> = const { Cell::new(None) };
    /// The names of the functions live when the last check asked for the demand set.
    static NAMES: RefCell<Vec<String>> = const { RefCell::new(vec![]) };
}

/// In [`finish`], what its tests ask of it.
pub(super) fn note_finish(cx: &mut crate::ctx::Ctx) {
    if let Some(path) = LATE_DEMAND.get() {
        cx.demand_std(path);
    }
}

/// Constructs `present` are about to demand their hooks.
pub(super) fn note_first_demand(present: ConstructSet) {
    BEFORE_HOOKS.set(present);
}

/// What the live set holds where a check asks for the demand set.
pub(super) fn note_demanded(cx: &crate::ctx::Ctx) {
    let live = &cx.lazy.live;
    PRESENT.set((live.present, live.impls.len()));
    NAMES.with_borrow_mut(|names| {
        names.clear();
        names.extend(
            live.live
                .iter()
                .filter_map(|d| match &cx.info[d.0 as usize] {
                    crate::defs::DefInfo::Fn(f) => Some(f.name.clone()),
                    _ => None,
                }),
        );
    });
}

/// [`PRESENT`] after a check of `src`, on a thread with sema's stack.
fn live(src: &str) -> (ConstructSet, usize) {
    checked(src).0
}

/// [`PRESENT`] and [`NAMES`] after a check of `src`, on a thread with sema's stack.
fn checked(src: &str) -> ((ConstructSet, usize), Vec<String>) {
    after_check(src, || (PRESENT.get(), NAMES.with_borrow(Vec::clone)))
}

/// `read` of the thread that checked `src`, which has sema's stack.
fn after_check<T: Send>(src: &str, read: impl Fn() -> T + Send + Sync) -> T {
    let l = programs::load_src(src);
    let (modules, root) = (&l.modules, l.root);
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .stack_size(SEMA_STACK_BYTES)
            .spawn_scoped(s, || {
                let opts = CheckOptions::default();
                let budget = SEMA_STACK_BUDGET;
                let (out, _) = crate::check_timed(modules, root, opts, &[], None, None, budget);
                let diags = out.1;
                assert!(
                    !diags.iter().any(|d| d.is_error()),
                    "{src}\n{}",
                    l.render(&diags)
                );
                read()
            });
        spawned.expect("sema thread").join().expect("sema thread")
    })
}

use Construct::{Dynamic, Format, IndexCheck, TextInt, TextUint};

#[test]
fn each_construct_is_found_where_live_code_has_it() {
    for (why, src, want) in [
        (
            "a template of a signed integer",
            "function main() { const n: i64 = 7; const s = `n=${n}`; if (s == \"\") { return; } }",
            &[TextInt][..],
        ),
        (
            "String() of an unsigned integer",
            "function main() { const n: u32 = 7; const s = String(n); if (s == \"\") { return; } }",
            &[TextUint],
        ),
        (
            "console.log of an integer",
            "function main() { const n: i32 = 7; console.log(n); }",
            &[Format],
        ),
        (
            "an array: the out-of-bounds message of an index",
            "function main() { const xs = [\"a\"]; const i: i64 = 0; const s = xs[i]; \
             if (s == \"b\") { return; } }",
            &[IndexCheck],
        ),
        (
            "a class converted to any: the format slot of its table",
            "class P { x = 1; }\nfunction main() { const a: any = new P(); if (a == 2) { return; } }",
            &[Format],
        ),
        (
            "a struct converted to any: the format slot of its table",
            "struct P { v: u32; }\nfunction main() { const p: P = { v: 1 }; const a: any = p; \
             if (a == 2) { return; } }",
            &[Format],
        ),
        (
            "a class converted to an interface: the format slot of its table",
            "interface I { f(): void; }\nclass P implements I { x = 1; f() { } }\n\
             function main() { const i: I = new P(); i.f(); }",
            &[Format],
        ),
        (
            "a class thrown and not caught",
            "class P { x = 1; }\nfunction main() { throw new P(); }",
            &[Format],
        ),
        (
            "a class printed",
            "class P { x = 1; }\nfunction main() { console.log(new P()); }",
            &[Format],
        ),
        (
            "an any: its numeric keys",
            "function main() { const a: any = \"s\"; if (a == \"t\") { return; } }",
            &[Dynamic],
        ),
        (
            "an integer converted to any",
            "function main() { const n: i32 = 5; const a: any = n; if (a == \"t\") { return; } }",
            &[Format],
        ),
        (
            "text of a type parameter, whose instance may be an integer of either sign",
            "function t<T>(v: T): string { return `${v}`; }\n\
             function main() { const s = t(\"a\"); if (s == \"\") { return; } }",
            &[TextInt, TextUint, Format],
        ),
        (
            "text of a type parameter's member type: an operation on it, which only the \
             conversion marks (a `T` itself is also converted to an `any`, which formats)",
            "function t<T extends { x: i64 }>(v: T[\"x\"]): string { return `${v}`; }\n\
             function main() { const s = t<{ x: i64 }>(1); if (s == \"\") { return; } }",
            &[TextInt, TextUint, Format],
        ),
        (
            "a promise that may reject with an integer, with no `throws` written on a function",
            "function f(p: Promise<string, i32>) { }\nfunction main() { }",
            &[Format],
        ),
        (
            "an integer thrown and not caught",
            "function main() { throw 5 as i32; }",
            &[Format],
        ),
        (
            "an integer thrown and not caught, in an async function",
            "async function main() { throw 5 as i32; }",
            &[Format],
        ),
        (
            "a function declared to throw an integer, whose body doesn't",
            "function f(): void throws i32 {}\nfunction main() { f(); }",
            &[Format],
        ),
        (
            "main declared to throw an integer, whose body doesn't",
            "function main(): void throws i32 {}",
            &[Format],
        ),
    ] {
        let present = after_check(src, || BEFORE_HOOKS.get());
        for c in want {
            assert!(
                present & c.bit() != 0,
                "{why}: no {}, found {present:#b} in\n{src}",
                c.name()
            );
        }
    }
}

#[test]
fn a_program_with_no_integer_has_no_construct() {
    let (present, _) = live("function main() { console.log(\"hi\"); }");
    assert_eq!(present, 0);
}

/// A class's or struct's table is built only where something can format it (`velt_vir`
/// `Cx::format_demanded`), so the type alone demands nothing (#2513).
#[test]
fn a_class_or_struct_nothing_formats_has_no_construct() {
    for src in [
        "struct P { v: u32; }\n\
         function main() { const p: P = { v: 1 }; console.log(p.v > 0); }",
        "class P { v: u32 = 1; }\nfunction main() { const p = new P(); console.log(p.v > 0); }",
        "class P { v: u32 = 1; }\nclass Q extends P { w: i64 = 2; }\n\
         function main() { const p: P = new Q(); console.log(p.v > 0); }",
    ] {
        assert_eq!(live(src).0, 0, "{src}");
    }
}

/// The program's own code is live anyway, so a construct can't show this: the row asserts that
/// the implementation is opened.
#[test]
fn an_interface_called_on_a_type_parameter_opens_its_implementations() {
    let (_, impls) = live(
        "interface I { f(): string; }\n\
         extend string implements I { f(): string { return \"\"; } }\n\
         function u<T extends I>(v: T): string { return v.f(); }\n\
         function main() { console.log(u(\"a\")); }",
    );
    assert!(impls > 0);
}

/// What sema makes after the last `reach` is walked in time for what it demands to be checked:
/// before ownership, not only where `live_defs` adds what is made after it.
#[test]
fn what_sema_makes_after_the_bodies_are_checked_is_live_before_ownership() {
    let (_, names) = checked(
        "interface Src {\n  get: () => string;\n}\n\
         class R {\n  toString(radix: i64 = 10): string {\n    return \"r\";\n  }\n}\n\
         function main() {\n  const s: any = Object.create(null);\n  \
         s.get = (): any => \"a\";\n  console.log((s as Src).get(), `${new R()}`);\n}\n",
    );
    for name in ["<any as () => string>", "R.<toString>"] {
        assert!(names.iter().any(|n| n == name), "no {name}");
    }
}

/// The diagnostics of a check of `src`, on a thread with sema's stack.
fn check_diags(src: &str, set: impl Fn() + Send + Sync) -> Vec<String> {
    let l = programs::load_src(src);
    let (modules, root) = (&l.modules, l.root);
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(crate::SEMA_STACK_BUDGET)
            .spawn_scoped(s, || {
                set();
                let opts = CheckOptions::default();
                let budget = SEMA_STACK_BUDGET;
                let (out, _) = crate::check_timed(modules, root, opts, &[], None, None, budget);
                out.1.iter().map(|d| d.message.clone()).collect()
            })
            .unwrap()
            .join()
            .unwrap()
    })
}

/// Lowering calls a root that mentions an `any` for an `any`. Printing an integer has an array
/// (a table of powers of ten), which must not open the `any` methods that take one, and with
/// them `Date` and the rest of what an `any` may hold.
#[test]
fn printing_an_integer_holds_no_any() {
    let (_, names) = checked("function main() {\n  const n: i64 = 7;\n  console.log(n);\n}\n");
    assert!(names.iter().any(|n| n.ends_with("::pushI64")), "{names:?}");
    for name in ["anyArr_push", "anyPrimitive", "Date.parse"] {
        assert!(!names.iter().any(|n| n.ends_with(name)), "{name} is live");
    }
}

#[test]
fn a_module_demanded_after_the_passes_is_an_internal_error() {
    let src = "function main() {\n  const n: i64 = 7;\n  console.log(`${n}`);\n}\n";
    let late = check_diags(src, || {
        LATE_DEMAND.set(Some("std/lang/hooks/no_such_module"));
    });
    assert!(
        late.iter()
            .any(|m| m.starts_with("ICE: std module `std/lang/hooks/no_such_module`")),
        "{late:?}"
    );
}
