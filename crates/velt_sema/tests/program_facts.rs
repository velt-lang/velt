//! Facts of the whole program live in tables of the program, not in the definitions they are
//! about (#2943): a std definition's checked form is the same whatever the program does with it.

mod common;

use common::programs::ok_src;
use velt_sema::hir::{Def, DefId, Program};

/// The std object type named `name` and its checked definition, as text.
fn class(p: &Program, name: &str) -> (DefId, String) {
    let (i, d) = (p.defs.iter().enumerate())
        .find(|(_, d)| matches!(d, Def::Adt(a) if a.name == name))
        .unwrap_or_else(|| panic!("no class {name}"));
    (DefId(i as u32), format!("{d:?}"))
}

#[test]
fn assigning_a_std_types_field_changes_the_program_not_the_type() {
    // std never assigns a `QueuingStrategy`'s field; this program does.
    let reads = ok_src(
        "import { QueuingStrategy } from \"velt/io\";
         function main() { const q: QueuingStrategy = { highWaterMark: 1 }; console.log(q.highWaterMark); }",
    );
    let writes = ok_src(
        "import { QueuingStrategy } from \"velt/io\";
         function main() { const q: QueuingStrategy = { highWaterMark: 1 }; q.highWaterMark = 2; console.log(q.highWaterMark); }",
    );
    let name = "std/io/streams/types::QueuingStrategy";
    let (d1, read_form) = class(&reads, name);
    let (d2, written_form) = class(&writes, name);
    assert_eq!(read_form, written_form);
    assert!(writes.assigned_objects.contains(&d2));
    assert!(!reads.assigned_objects.contains(&d1));
}

/// The function named `name` and its checked definition, as text.
fn function(p: &Program, name: &str) -> (DefId, String) {
    let (i, d) = (p.defs.iter().enumerate())
        .find(|(_, d)| matches!(d, Def::Fn(f) if f.name == name))
        .unwrap_or_else(|| panic!("no function {name}"));
    (DefId(i as u32), format!("{d:?}"))
}

#[test]
fn putting_a_function_in_shared_changes_the_program_not_the_closures() {
    // `make`'s closure changes what it captured; only the second program puts a function in
    // `shared(...)`, which makes the program find such closures (`Program::changes_held`).
    let make = "function make(): () => void { let n: i64 = 0; return () => { n += 1; }; }";
    let plain = ok_src(&format!("{make}\nasync function main() {{ make()(); }}"));
    let sharing = ok_src(&format!(
        "{make}\nasync function main() {{ make()(); const s = shared(() => {{}}); console.log(s == null); }}"
    ));
    let name = "make::{closure#0}";
    let (d1, plain_form) = function(&plain, name);
    let (d2, sharing_form) = function(&sharing, name);
    assert_eq!(plain_form, sharing_form);
    assert!(sharing.changes_held.contains(&d2));
    assert!(!plain.changes_held.contains(&d1));
}
