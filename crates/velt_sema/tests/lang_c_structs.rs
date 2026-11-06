//! C structs (`declare c struct`, docs/internals/design/c-ffi.md "C structs"): fields in C's
//! order of types C has, and nothing else; in Velt a value, copied and never counted, whose
//! fields are read-only.

mod common;

use common::programs::{err_src, ok_src};
use velt_sema::hir::UseMode;

const GEOMETRY: &str = "declare c struct CGPoint { x: f64; y: f64; }
declare c struct CGSize { width: f64; height: f64; }
declare c struct CGRect { origin: CGPoint; size: CGSize; }";

#[test]
fn c_structs_are_values_built_from_literals() {
    ok_src(&format!(
        "{GEOMETRY}
         declare c struct Mixed {{ a: u8; b: i32; c: [f32, f32, f32]; m: [[u8, u8], [u8, u8]]; ok: bool; n: isize; }}
         function moved(r: CGRect, dx: f64): CGRect {{
           return {{ origin: {{ x: r.origin.x + dx, y: r.origin.y }}, size: r.size }};
         }}
         function main() {{
           const r: CGRect = {{ origin: {{ x: 1, y: 2 }}, size: {{ width: 3, height: 4 }} }};
           const s = r;
           const t = moved(s, 10);
           const u = {{ ...t, size: {{ width: 0, height: 0 }} }} as CGRect;
           const rs: CGRect[] = [r, t, u];
           const m: Mixed = {{ a: 7, b: -3, c: [1.5, 2.5, 3.5], m: [[1, 2], [3, 4]], ok: true, n: 0 }};
           console.log(r.size.width, s.origin.x, rs.length, m.c[2]);
         }}"
    ));
}

#[test]
fn a_c_struct_is_copy() {
    // Every read of a Copy value copies it: no move, no borrow, nothing counted.
    let p = ok_src(&format!(
        "{GEOMETRY}
         function take(r: CGRect): f64 {{ return r.size.width; }}
         function main() {{
           const r: CGRect = {{ origin: {{ x: 1, y: 2 }}, size: {{ width: 3, height: 4 }} }};
           const s = r;
           const rs: CGRect[] = [r, s];
           console.log(take(r), take(s), rs.length);
         }}"
    ));
    let main = common::fn_named(&p, "main");
    for name in ["r", "s"] {
        let modes = common::hir_walk::uses_of(main, name);
        assert!(
            !modes.is_empty() && modes.iter().all(|m| *m == UseMode::Copy),
            "`{name}`: {modes:?}"
        );
    }
}

#[test]
fn a_c_struct_has_fields_only() {
    let e = err_src(
        "interface Q {}
         declare c struct Bad<T> implements Q {
           o?: i32; d: i32 = 3; static k: i32 = 1; private z: i32;
           m(): void {}
         }
         function main() {}",
    );
    for what in [
        "C struct `Bad` can't have type parameters",
        "C struct `Bad` can't have `implements`",
        "C struct `Bad` can't have methods",
        "C struct `Bad` can't have optional fields",
        "C struct `Bad` can't have field defaults",
        "C struct `Bad` can't have static fields",
        "C struct `Bad` can't have private fields",
    ] {
        assert!(e.contains(what), "missing `{what}`:\n{e}");
    }
}

#[test]
fn a_c_struct_has_fields_of_types_c_has() {
    let e = err_src(
        "declare c struct Bad { s: string; n: number[]; t: [i32, f32]; h: f16; o: i32 | null; ok: i32; }
         function main() {}",
    );
    for field in ["`s`", "`n`", "`t`", "`h`", "`o`"] {
        assert!(
            e.contains(&format!("field {field} of C struct `Bad` has type")),
            "missing {field}:\n{e}"
        );
    }
    assert!(!e.contains("field `ok`"), "{e}");
    assert!(e.contains("tuples of one of these as C arrays"), "{e}");
}

/// `Ptr<T> | null` is an option of the address in Velt, larger than C's pointer: pointer fields
/// wait for their own storage.
#[test]
fn pointer_fields_wait_for_their_layer() {
    let e = std_diagnostics(
        "import { Ptr } from \"velt/ffi\";
         declare c struct Node { value: i32; next: Ptr<Node> | null; first: Ptr<u8>; ps: [Ptr<u8>, Ptr<u8>]; }
         function main() {}",
    );
    for field in ["`next`", "`first`", "`ps`"] {
        assert!(
            e.contains(&format!("field {field} of C struct `Node` is a pointer")),
            "missing {field}:\n{e}"
        );
    }
    assert!(e.contains("stored as C's address"), "{e}");
}

/// `src` checked as std code (which may use velt/ffi's raw API); the rendered diagnostics.
fn std_diagnostics(src: &str) -> String {
    let mut l = common::programs::load_src(src);
    let root = l.root;
    l.modules[root].is_std = true;
    let (_, d) = l.check();
    l.render(&d)
}

#[test]
fn a_c_struct_is_never_empty_or_recursive() {
    let e = err_src("declare c struct Empty {} function main() {}");
    assert!(e.contains("C struct `Empty` has no fields"), "{e}");
    assert!(e.contains("`declare c type`"), "{e}");
    let e = err_src("declare c struct Rec { r: Rec; } function main() {}");
    assert!(e.contains("recursive type `Rec` has infinite size"), "{e}");
    assert!(e.contains("a C struct can't hold itself"), "{e}");
    assert!(
        !e.contains("Ptr<Rec>"),
        "pointer fields aren't there yet: {e}"
    );
}

#[test]
fn a_c_structs_fields_are_read_only() {
    let e = err_src(&format!(
        "{GEOMETRY}
         function main() {{ const p: CGPoint = {{ x: 1, y: 2 }}; p.x = 3; console.log(p.x); }}"
    ));
    assert!(
        e.contains("cannot assign to `x`: it is a readonly field"),
        "{e}"
    );
    assert!(e.contains("make a changed copy (`{ ...v, x: … }`)"), "{e}");
}

#[test]
fn c_structs_have_no_identity_to_compare() {
    let e = err_src(&format!(
        "{GEOMETRY}
         function main() {{ const p: CGPoint = {{ x: 1, y: 2 }}; console.log(p == p); }}"
    ));
    assert!(
        e.contains("cannot apply binary operator `==` to type `CGPoint`"),
        "{e}"
    );
    assert!(e.contains("compare its fields"), "{e}");
}

/// A value holding a C struct inline (an option, a tuple) would compare it too, so `==` on one
/// is the same error. Comparing with `null` stays a test for nothing, and an array of C structs
/// is an object, compared by identity as every array is.
#[test]
fn values_holding_c_structs_have_nothing_to_compare_either() {
    for (decl, shown) in [
        (
            "const a: CGPoint | null = pick(p); const b: CGPoint | null = pick(p);",
            "CGPoint | null",
        ),
        (
            "const a: [CGPoint, i32] = [p, 1]; const b: [CGPoint, i32] = [p, 2];",
            "[CGPoint, i32]",
        ),
    ] {
        let e = err_src(&format!(
            "{GEOMETRY}
             function pick(p: CGPoint): CGPoint | null {{ return p.x > 0 ? p : null; }}
             function main() {{ const p: CGPoint = {{ x: 1, y: 2 }}; {decl} console.log(a == b, a != b); }}"
        ));
        assert!(
            e.contains(&format!(
                "cannot apply binary operator `==` to type `{shown}`"
            )),
            "`{shown}`: {e}"
        );
        assert!(e.contains("holds a C struct"), "`{shown}`: {e}");
    }
    ok_src(&format!(
        "{GEOMETRY}
         function main() {{
           const a: CGPoint | null = null;
           const p: CGPoint = {{ x: 1, y: 2 }};
           const xs: CGPoint[] = [p];
           const ys = xs;
           console.log(a == null, a !== null, xs == ys);
         }}"
    ));
}

#[test]
fn a_c_struct_is_not_made_with_new() {
    let e = err_src(&format!(
        "{GEOMETRY}
         function main() {{ const p = new CGPoint(); console.log(p.x); }}"
    ));
    assert!(e.contains("`CGPoint` is a struct, not a class"), "{e}");
}
