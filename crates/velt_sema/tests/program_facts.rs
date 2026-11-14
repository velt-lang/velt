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
