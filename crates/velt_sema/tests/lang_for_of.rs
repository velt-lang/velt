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
    // Semantics stage 2: an element pushed elsewhere is shared.
    ok_src(
        "function load(): i64[][] { return [[1], [2]]; }
         function main() { const xs = load(); const out: i64[][] = [];
           for (const s of xs) { out.push(s); } }",
    );
    let r = err_src(&format!(
        "{LOAD} function main() {{ for (const s of load()) {{ s = \"z\"; }} }}"
    ));
    assert!(r.contains("cannot assign"), "{r}");
}

/// `ForOf::share_elems` of the loops in `f`'s body, outer loops first.
fn share_elems(p: &velt_sema::hir::Program, f: &str) -> Vec<bool> {
    use velt_sema::hir::{Block, StmtKind as S};
    fn walk(b: &Block, out: &mut Vec<bool>) {
        for s in &b.stmts {
            if let S::ForOf {
                body, share_elems, ..
            } = &s.kind
            {
                out.push(*share_elems);
                walk(body, out);
            }
        }
    }
    let mut out = vec![];
    walk(&common::hir_walk::func(p, f).body.block, &mut out);
    out
}

/// A body that replaces (a part of) the element a binding refers to (#1863) makes the
/// bindings take shares; one that only changes the element in place, or replaces a part no
/// binding refers to, leaves the loop as it is (no count per element).
#[test]
fn bindings_share_elements_the_body_replaces() {
    let p = ok_src(
        "class V { n: i64 = 0; w: V[] = []; }
         function replaced(vs: V[]) { for (const v of vs) { vs[0] = new V(); console.log(v.n); } }
         function compound(ss: string[]) { for (const s of ss) { ss[0] += \"x\"; console.log(s); } }
         function nested(g: V[][]) { for (const row of g) { for (const v of row) { row[0] = new V(); console.log(v.n); } } }
         function part(ps: [V, V][]) { for (const [a, b] of ps) { ps[0][1] = new V(); console.log(a.n, b.n); } }
         function in_place(vs: V[]) { for (const v of vs) { v.n = 1; vs[0].n = 2; vs[0].w.push(new V()); console.log(v.n); } }
         function other_part(ps: [V, V][]) { for (const [a] of ps) { ps[0][1] = new V(); console.log(a.n); } }
         function reads(vs: V[]) { for (const v of vs) { console.log(v.n, vs[0].n, vs.length); } }
         function numbers(ns: i64[]) { for (const n of ns) { ns[0] = 5; console.log(n); } }
         function main() {}",
    );
    for f in ["replaced", "compound", "part"] {
        assert_eq!(share_elems(&p, f), [true], "{f}");
    }
    assert_eq!(share_elems(&p, "nested"), [false, true]);
    for f in ["in_place", "other_part", "reads", "numbers"] {
        assert_eq!(share_elems(&p, f), [false], "{f}");
    }
}
