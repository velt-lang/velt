//! Interface summaries (`velt_sema::interface`): two versions of a program that differ in one
//! function's body have the same summaries, except where the edit changes what other code
//! observes of that function or a program-wide fact.

mod common;

use common::programs::ok_src;
use velt_sema::hir::Def;
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
    // `main` prints the same in both versions: printing a number and a string reach
    // different std hooks, which is another change.
    let before = "function f() { return 1; }
                  function main() { f(); console.log(\"x\"); }";
    let after = "function f() { return \"one\"; }
                 function main() { f(); console.log(\"x\"); }";
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
    // Exactly the new closure: no fact moves.
    assert_eq!(changes(before, after), vec![changed_fn("f::{closure#0}")]);
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

#[test]
fn reading_a_member_of_an_any_changes_the_dynamic_tables() {
    let main = "function main() { f(1); }";
    let before = format!("function f(x: any) {{ console.log(x); }}\n{main}");
    let after = format!("function f(x: any) {{ console.log(x.a); }}\n{main}");
    assert_eq!(
        changes(&before, &after),
        vec![Change::Fact("dynamic members")]
    );
}

#[test]
fn a_closure_that_starts_assigning_what_it_captured_changes_its_capture_mode() {
    let main = "function main() { f(); }";
    let before =
        format!("function f() {{ let n = 0; const g = (): i64 => n; console.log(g()); }}\n{main}");
    let after = format!(
        "function f() {{ let n = 0; const g = (): i64 => {{ n += 1; return n; }}; console.log(g()); }}\n{main}"
    );
    assert_eq!(changes(&before, &after), vec![changed_fn("f::{closure#0}")]);
}

#[test]
fn a_write_through_a_shared_holder_makes_its_type_counted() {
    // `unused` shares an `Inner` held in an `Outer`, so the write in `main` that may replace
    // the holder is accepted, and lowering counts `Inner` (golden
    // lang/assign_target_shared_holder_unused).
    let held = "type Inner = { v: number };
                type Outer = { inner: Inner };
                function unused(o: Outer): Inner { const k = o.inner; o.inner.v = 1; return k; }";
    let main = |write: &str| {
        format!(
            "{held}
             function main() {{
               const o: Outer = {{ inner: {{ v: 0 }} }};
               const g = (): number => {{ o.inner = {{ v: 100 }}; return 13; }};
               {write}
             }}"
        )
    };
    let before = main("o.inner.v = 13; console.log(o.inner.v, g());");
    let after = main("o.inner.v = g(); console.log(o.inner.v);");
    assert_eq!(
        changes(&before, &after),
        vec![Change::Fact("counted types")]
    );
}

#[test]
fn reading_import_meta_adds_to_the_module_state() {
    let main = "function main() { f(); }";
    let before = format!("function f() {{ console.log(1); }}\n{main}");
    let after = format!("function f() {{ console.log(import.meta.url.length > 0); }}\n{main}");
    let found = changes(&before, &after);
    assert!(found.contains(&Change::Fact("module state")), "{found:?}");
    // `std/lang/import_meta` is loaded only for the second version: its functions are part of
    // the reached std, not listed one by one.
    assert!(found.contains(&Change::Fact("reached std")), "{found:?}");
    assert!(
        !found.iter().any(|c| matches!(c, Change::Fn(_))),
        "{found:?}"
    );
}

#[test]
fn a_callback_that_reaches_a_member_of_an_any_is_no_longer_lock_free() {
    // `twice` has properties (`setup`), so the module lock is taken around callbacks that may
    // reach them (lang_lock_free_callbacks.rs). Only the closure's lock-free mark changes.
    let program = |call: &str| {
        format!(
            "class Plain {{ n: number = 0; f: any = null; }}
             function readK(x: any): number {{ return x.k; }}
             function twice(n: number): number {{ return n * 2; }}
             function setup() {{ const d0: any = twice; d0.k = 1; }}
             function f(m: shared<Mutex<Plain>>) {{ m.with((b) => {{ b.n += {call}; }}); }}
             function main() {{ setup(); f(shared(new Mutex(new Plain()))); }}"
        )
    };
    let found = changes(&program("twice(1)"), &program("readK(b.f)"));
    assert_eq!(found, vec![changed_fn("f::{closure#0}")]);
}

#[test]
fn a_decorator_in_a_body_changes_the_decorators() {
    let dec = "declare decorator function mark(): FunctionDecorator;";
    let program = |mark: &str| {
        format!(
            "{dec}
             function f() {{ {mark} function g(): i64 {{ return 1; }} console.log(g()); }}
             function main() {{ f(); }}"
        )
    };
    assert_eq!(
        changes(&program(""), &program("@mark")),
        vec![Change::Fact("decorators")]
    );
}

#[test]
fn functions_of_the_same_name_are_all_compared() {
    // Names are unique, but one built from display text could repeat: a change of the first of
    // two functions of one name must not be hidden by the second.
    let src = "function f(): i64 { return 1; }
               function main() { console.log(f()); }";
    let with_twin = |change_first: bool| {
        let mut p = ok_src(src);
        let i = (p.defs.iter())
            .position(|d| matches!(d, Def::Fn(f) if f.name == "f"))
            .expect("f");
        let twin = p.defs[i].clone();
        if change_first {
            let Def::Fn(f) = &mut p.defs[i] else {
                unreachable!()
            };
            f.is_async = true;
        }
        p.defs.push(twin);
        Summary::of(&p)
    };
    let found = with_twin(false).changes(&with_twin(true));
    assert_eq!(found, vec![changed_fn("f")]);
}

#[test]
fn storing_a_parameter_changes_its_pass_mode() {
    // Borrowed, then owned (moved into `keep`); the parameter is modified in neither.
    let cls = "class P { x: i64 = 0; }
               let keep: P[] = [];";
    let program = |body: &str| {
        format!("{cls}\nfunction f(p: P) {{ {body} }}\nfunction main() {{ f(new P()); }}")
    };
    let found = changes(&program("console.log(p.x);"), &program("keep.push(p);"));
    assert!(found.contains(&changed_fn("f")), "{found:?}");
}

#[test]
fn using_a_captured_variable_after_the_closure_makes_the_capture_share() {
    // The closure escapes either way (its capture is owned); used afterwards, the enclosing
    // variable is shared with it instead of moved into it.
    let program = |after: &str| {
        format!(
            "let fs: (() => i64)[] = [];
             function f() {{ const s = \"ab\"; const g = (): i64 => s.length; fs.push(g); {after} }}
             function main() {{ f(); }}"
        )
    };
    let found = changes(&program(""), &program("console.log(s);"));
    assert!(found.contains(&changed_fn("f::{closure#0}")), "{found:?}");
}

#[test]
fn a_std_module_loaded_for_one_version_only_is_reached_std() {
    // Even where lazy std checks every function of the new module, so the unchecked set
    // doesn't tell: the second version's unchecked set is made the first one's here.
    let main = "function main() { f(); }";
    // Both print a boolean: printing a number reaches other std hooks.
    let before = ok_src(&format!("function f() {{ console.log(1 > 0); }}\n{main}"));
    let mut after = ok_src(&format!(
        "function f() {{ console.log(import.meta.url.length > 0); }}\n{main}"
    ));
    // Lazy std leaves functions and module constants unchecked.
    let name = |p: &velt_sema::hir::Program, i: usize| match &p.defs[i] {
        Def::Fn(f) => Some(f.name.clone()),
        Def::Global(g) => Some(g.name.clone()),
        _ => None,
    };
    let unchecked: std::collections::HashSet<String> = (before.unchecked.iter())
        .filter_map(|d| name(&before, d.0 as usize))
        .collect();
    after.unchecked = (0..after.defs.len())
        .filter(|&i| name(&after, i).is_some_and(|n| unchecked.contains(&n)))
        .map(|i| velt_sema::hir::DefId(i as u32))
        .collect();
    let found = Summary::of(&before).changes(&Summary::of(&after));
    assert!(found.contains(&Change::Fact("reached std")), "{found:?}");
    assert!(
        !found.iter().any(|c| matches!(c, Change::Fn(_))),
        "{found:?}"
    );
}

#[test]
fn calling_a_generators_throw_changes_the_flags() {
    // `Program::generator_throw`: resume functions then handle an error thrown in at each yield.
    let program = |body: &str| {
        format!(
            "function* g(): Generator<number> {{ yield 1; }}
             function f() {{ const it = g(); {body} }}
             function main() {{ f(); }}"
        )
    };
    let before = program("console.log(it.next().value);");
    let after = program("try { it.throw(new Error(\"x\")); } catch (e) { console.log(e); }");
    assert_eq!(changes(&before, &after), vec![Change::Fact("flags")]);
}

#[test]
fn the_first_any_in_a_body_adds_an_implementation() {
    // `impl<T> any for T` (`Program::any_impl`) exists once the program uses `any`; the other
    // facts it brings along (the std `any` reaches) are not what this test is about.
    let main = "function main() { f(); }";
    let before = format!("function f() {{ const x = 1; console.log(x); }}\n{main}");
    let after = format!("function f() {{ const x: any = 1; console.log(x); }}\n{main}");
    let found = changes(&before, &after);
    assert!(
        found.contains(&Change::Fact("implementations")),
        "{found:?}"
    );
}
