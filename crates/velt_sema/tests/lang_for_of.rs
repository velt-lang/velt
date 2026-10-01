//! `for...of` over a temporary array of non-Copy elements consumes it (owned elements);
//! over a place it borrows them.

mod common;

use common::programs::{err_src, ok_src};

const LOAD: &str = "function load(): string[] { return [\"a\", \"b\"]; }";

/// Strings are values: an element of a borrowed `string[]` can be pushed elsewhere (a copy).
#[test]
fn string_elements_of_places_are_copied() {
    ok_src(&format!(
        "{LOAD} function main() {{ const xs = load(); const out: string[] = [];
           for (const s of xs) {{ out.push(s); }} console.log(xs, out); }}"
    ));
}

#[test]
fn temporaries_are_consumed_and_elements_can_be_moved() {
    ok_src(&format!(
        "{LOAD} function main() {{ const out: string[] = [];
           for (const s of load()) {{ out.push(s); }}
           for (let s of [\"x\"]) {{ s += \"!\"; out.push(s); }}
           for (const [a, b] of [[\"p\", \"q\"]]) {{ out.push(b); out.push(a); }}
           console.log(out); }}"
    ));
}

#[test]
fn places_are_still_borrowed() {
    let r = err_src(
        "function load(): i64[][] { return [[1], [2]]; }
         function main() { const xs = load(); const out: i64[][] = [];
           for (const s of xs) { out.push(s); } }",
    );
    assert!(r.contains("which borrows an array element"), "{r}");
    let r = err_src(&format!(
        "{LOAD} function main() {{ for (const s of load()) {{ s = \"z\"; }} }}"
    ));
    assert!(r.contains("cannot assign"), "{r}");
}
