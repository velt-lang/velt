//! Each construct of `hook_demand::Construct` is found where live code has it. Once one integer
//! hook is live its std code brings the others, so what a program demands can't tell these apart
//! (#2312's review). Each row asserts the constructs found, in a program where the construct
//! named is gone if the code that finds it is: the walk stops as soon as every hook is demanded,
//! so a construct std's code would also bring is not found later.

use std::cell::{Cell, RefCell};

use velt_common::std_hooks::{Construct, ConstructSet};

use crate::script_locals::sema_tests::programs;
use crate::{CheckOptions, SEMA_STACK_BUDGET, SEMA_STACK_BYTES};

thread_local! {
    /// What the live set held when the last check on this thread asked for the demand set: its
    /// constructs and how many implementations it opened.
    pub(super) static PRESENT: Cell<(ConstructSet, usize)> = const { Cell::new((0, 0)) };
    /// Work the set out in full, as a debug compiler outside these tests does.
    pub(super) static EXACT: Cell<bool> = const { Cell::new(false) };
    /// The names of the functions live when the last check asked for the demand set.
    static NAMES: RefCell<Vec<String>> = const { RefCell::new(vec![]) };
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
    checked(src, false).0
}

/// [`PRESENT`] and [`NAMES`] after a check of `src`, on a thread with sema's stack.
fn checked(src: &str, exact: bool) -> ((ConstructSet, usize), Vec<String>) {
    let l = programs::load_src(src);
    let (modules, root) = (&l.modules, l.root);
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .stack_size(SEMA_STACK_BYTES)
            .spawn_scoped(s, || {
                EXACT.set(exact);
                let opts = CheckOptions::default();
                let budget = SEMA_STACK_BUDGET;
                let (out, _) = crate::check_timed(modules, root, opts, &[], None, None, budget);
                let diags = out.1;
                assert!(
                    !diags.iter().any(|d| d.is_error()),
                    "{src}\n{}",
                    l.render(&diags)
                );
                (PRESENT.get(), NAMES.with_borrow(Vec::clone))
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
            "a class: the format slot of its table",
            "class P { x = 1; }\nfunction main() { const p = new P(); if (p.x == 2) { return; } }",
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
        (
            "a function value: lowering's helpers that take one open, and they format",
            "function f(): void throws string { }\n\
             function main() { const h = f; if (h == null) { return; } }",
            &[Format],
        ),
    ] {
        let (present, _) = live(src);
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
        true,
    );
    for name in ["<any as () => string>", "R.<toString>"] {
        assert!(names.iter().any(|n| n == name), "no {name}");
    }
}
