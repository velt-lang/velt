//! The error for `shared(...)` of a value something else still holds: for a field, the note on
//! taking it out without a copy names the field as code; for an element, whose place text is
//! `xs[..]` and not code, there is no such note.

mod common;

use common::programs::err_src;

const TAKE: &str = "take it out without a copy";

#[test]
fn a_kept_field_gets_the_way_to_hand_it_over_without_a_copy() {
    let err = err_src(
        "class Index { data: f32[] = []; freeze(): shared<f32[]> { return shared(this.data); } }
         function main() { console.log(new Index().freeze().length); }",
    );
    assert!(err.contains("`this.data` stays where it is held"), "{err}");
    assert!(
        err.contains("`const v = this.data; this.data = …;` (a new value), then `shared(v)`"),
        "{err}"
    );
}

#[test]
fn a_kept_element_gets_no_snippet_that_is_not_code() {
    let err = err_src(
        "function main() { const rows: f32[][] = [[1, 2]]; const s = shared(rows[0]);
           console.log(s.length, rows.length); }",
    );
    assert!(err.contains("`rows[..]` stays where it is held"), "{err}");
    assert!(!err.contains(TAKE), "{err}");
}
