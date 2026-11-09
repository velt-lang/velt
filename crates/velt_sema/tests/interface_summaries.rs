//! Interface summaries (`velt_sema::interface`): two versions of a program that differ in one
//! function's body have the same summaries, except where the edit changes what other code
//! observes of that function or a program-wide fact.

mod common;

use common::programs::ok_src;
use velt_sema::interface::{is_closure_of, Change, Summary};

/// The changes from the program `before` to the program `after`.
fn changes(before: &str, after: &str) -> Vec<Change> {
    Summary::of(&ok_src(before)).changes(&Summary::of(&ok_src(after)))
}

fn changed_fn(name: &str) -> Change {
    Change::Fn(name.to_string())
}

#[test]
fn the_same_program_has_the_same_summary() {
    let src = "function f(xs: i64[]): i64 { return xs.length; }
               function main() { console.log(f([1, 2])); }";
    assert_eq!(changes(src, src), vec![]);
}

#[test]
fn a_body_edit_that_keeps_the_interface_changes_nothing() {
    // The edit makes types the other version doesn't have (a tuple, an array of them, a map
    // instance) before `g` is checked: their type ids move, the summaries don't.
    let main = "function main() { const m = new Map<string, i64>(); m.set(\"a\", 1);
                  console.log(g(f(m.size)), h(2)); }";
    let before = format!(
        "function f(n: i64): string {{ return `${{n}}`; }}
         function g(s: string): i64 {{ return s.length; }}
         function h(n: i64) {{ const t: [string, boolean, i64] = [\"a\", n > 0, n]; return t; }}
         {main}"
    );
    let after = format!(
        "function f(n: i64): string {{
           const m = new Map<string, [i64, boolean]>();
           m.set(\"a\", [n, true]);
           const t: [boolean, i64] = [false, m.size];
           return `${{n}} ${{t[1]}}`;
         }}
         function g(s: string): i64 {{ return s.length; }}
         function h(n: i64) {{ const t: [string, boolean, i64] = [\"a\", n > 0, n]; return t; }}
         {main}"
    );
    assert_eq!(changes(&before, &after), vec![]);
}

#[test]
fn reaching_more_of_std_is_a_program_wide_change() {
    let before = "function f(n: i64): string { return `${n}`; }
                  function main() { console.log(f(1)); }";
    let after = "function f(n: i64): string { const m = new Map<string, i64>(); m.set(\"a\", n);
                   return `${m.size}`; }
                 function main() { console.log(f(1)); }";
    let found = changes(before, after);
    assert!(found.contains(&Change::Fact("reached std")), "{found:?}");
    // Newly checked std functions are part of the fact, not listed one by one (with their
    // closures). Functions checked in both versions may still change: whether a callback is
    // lock-free depends on all the code the program reaches.
    let listed = |name: &str| {
        found
            .iter()
            .any(|c| matches!(c, Change::Fn(n) if n.starts_with(name)))
    };
    assert!(!listed("std/prelude/map::Map.set"), "{found:?}");
    assert!(!listed("std/lang/map/"), "{found:?}");
}

#[test]
fn an_inferred_return_type_is_part_of_the_interface() {
    let before = "function f() { return 1; }
                  function main() { console.log(f()); }";
    let after = "function f() { return \"one\"; }
                 function main() { console.log(f()); }";
    assert_eq!(changes(before, after), vec![changed_fn("f")]);
}

#[test]
fn modifying_a_parameter_is_part_of_the_interface() {
    let before = "function f(xs: i64[]) { console.log(xs.length); }
                  function main() { const xs: i64[] = [1]; f(xs); console.log(xs); }";
    let after = "function f(xs: i64[]) { xs.push(2); }
                 function main() { const xs: i64[] = [1]; f(xs); console.log(xs); }";
    assert!(changes(before, after).contains(&changed_fn("f")));
}

#[test]
fn the_error_set_is_part_of_the_interface() {
    let before = "function f(n: i64): i64 { return n; }
                  function main() { try { console.log(f(1)); } catch (e) { console.log(e); } }";
    let after = "function f(n: i64): i64 { if (n > 0) { throw new Error(\"no\"); } return n; }
                 function main() { try { console.log(f(1)); } catch (e) { console.log(e); } }";
    assert!(changes(before, after).contains(&changed_fn("f")));
}

#[test]
fn assigning_a_field_in_a_body_changes_the_declarations() {
    let before = "class P { x: i64 = 0; }
                  function f(p: P) { console.log(p.x); }
                  function main() { const p = new P(); f(p); }";
    let after = "class P { x: i64 = 0; }
                 function f(p: P) { p.x = 1; console.log(p.x); }
                 function main() { const p = new P(); f(p); }";
    let found = changes(before, after);
    assert!(found.contains(&Change::Fact("declarations")), "{found:?}");
}

#[test]
fn a_closure_added_to_a_body_is_that_functions_own() {
    let before = "function f(xs: i64[]): i64 { return xs[0] + xs[1]; }
                  function main() { console.log(f([1, 2])); }";
    let after = "function f(xs: i64[]): i64 { const add = (a: i64, b: i64): i64 => a + b;
                   return add(xs[0], xs[1]); }
                 function main() { console.log(f([1, 2])); }";
    let found = changes(before, after);
    assert!(!found.is_empty());
    for c in &found {
        match c {
            Change::Fn(name) => assert!(is_closure_of(name, "f"), "{found:?}"),
            Change::Fact(_) => {}
        }
    }
    assert!(!found.contains(&changed_fn("f")), "{found:?}");
}

#[test]
fn a_closure_type_in_a_signature_is_named_not_numbered() {
    // `f`'s new closure is defined before `mk`'s: `mk`'s closure moves to another `DefId`.
    let rest = "function mk() { const k = 2; return (x: i64): i64 => x * k; }
                function main() { console.log(f([1, 2]), mk()(3)); }";
    let before = format!("function f(xs: i64[]): i64 {{ return xs[0]; }}\n{rest}");
    let after = format!(
        "function f(xs: i64[]): i64 {{ const g = (i: i64): i64 => xs[i]; return g(0); }}\n{rest}"
    );
    let found = changes(&before, &after);
    assert_eq!(found, vec![changed_fn("f::{closure#0}")]);
}

#[test]
fn closures_are_named_after_their_function() {
    assert!(is_closure_of("f::{closure#0}", "f"));
    assert!(is_closure_of("f::{closure#1}::{closure#0}", "f"));
    assert!(is_closure_of("std/fs::f::{closure#2}", "std/fs::f"));
    assert!(!is_closure_of("fg::{closure#0}", "f"));
    assert!(!is_closure_of("f", "f"));
}
