//! C structs (`declare c struct`, docs/internals/design/c-ffi.md "C structs"): fields in C's
//! order of types C has, and nothing else; in Velt a value, copied and never counted, whose
//! fields are read-only and which compares by content.

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
         declare c struct Mixed {{ a: u8; b: i32; ok: bool; n: isize; r: CGRect; }}
         function moved(r: CGRect, dx: f64): CGRect {{
           return {{ origin: {{ x: r.origin.x + dx, y: r.origin.y }}, size: r.size }};
         }}
         function main() {{
           const r: CGRect = {{ origin: {{ x: 1, y: 2 }}, size: {{ width: 3, height: 4 }} }};
           const s = r;
           const t = moved(s, 10);
           const u = {{ ...t, size: {{ width: 0, height: 0 }} }} as CGRect;
           const rs: CGRect[] = [r, t, u];
           const m: Mixed = {{ a: 7, b: -3, ok: true, n: 0, r: u }};
           console.log(r.size.width, s.origin.x, rs.length, m.r.origin.x);
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
    assert!(e.contains("other C structs"), "{e}");
}

/// A tuple's storage isn't fixed to C's: lowering counts a tuple type that the program writes
/// in place and copies (boxing/tuples.rs), anywhere in the program, which would make the field
/// a pointer, and an element write would get past the read-only fields. C arrays wait for
/// `T[N]`: a C array field, one written through an element after a copy, and arrays of
/// structs and of arrays are refused at the field.
#[test]
fn c_arrays_wait_for_fixed_size_arrays() {
    let e = err_src(&format!(
        "{GEOMETRY}
         declare c struct R {{ m: [f32, f32, f32]; b: u8; }}
         declare c struct T {{ ps: [CGPoint, CGPoint]; m: [[u8, u8], [u8, u8]]; }}
         const r0: R = {{ m: [1, 2, 3], b: 4 }};
         function unrelated(): f32 {{ let t: [f32, f32, f32] = [1, 2, 3]; let u = t; u[0] = 9; return u[0]; }}
         function main() {{ const a: R = r0; const b = a; a.m[0] = 9; console.log(a.m[0], b.m[0], unrelated()); }}"
    ));
    for field in [
        "`m` of C struct `R`",
        "`ps` of C struct `T`",
        "`m` of C struct `T`",
    ] {
        assert!(
            e.contains(&format!("field {field} is a C array")),
            "missing {field}:\n{e}"
        );
    }
    assert!(e.contains("fixed-size arrays (`T[N]`"), "{e}");
}

/// A field whose type is already an error, or holds one, is reported once.
#[test]
fn an_unknown_field_type_is_reported_once() {
    let e = err_src(
        "declare c struct X { a: [Nope, Nope]; b: Nope; d: Map<Nope, i32>; e: Nope[] | null; } function main() {}",
    );
    assert!(!e.contains("which C doesn't have"), "{e}");
    assert!(!e.contains("is a C array"), "{e}");
    assert!(e.contains("Nope"), "{e}");
}

/// C has enums and opaque types, but neither crosses by value as Velt has them: a Velt enum is
/// an `i64` where C's is usually an `int`, and a `declare c type` exists only behind a pointer.
#[test]
fn enums_and_c_types_by_value_say_what_to_declare() {
    let e = err_src(
        "enum En { A, B }
         declare c type H;
         declare c struct X { e: En; h: H; }
         function main() {}",
    );
    assert!(e.contains("field `e` of C struct `X` has type `En`"), "{e}");
    assert!(
        e.contains("declare the field as the C enum's integer type (`i32`)"),
        "{e}"
    );
    assert!(e.contains("field `h` of C struct `X` has type `H`"), "{e}");
    assert!(e.contains("a `Ptr<T>` field comes in a later layer"), "{e}");
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
    for field in ["`next`", "`first`"] {
        assert!(
            e.contains(&format!("field {field} of C struct `Node` is a pointer")),
            "missing {field}:\n{e}"
        );
    }
    assert!(
        e.contains("field `ps` of C struct `Node` is a C array"),
        "{e}"
    );
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

/// Every write reached through a C struct's fields is refused, whatever holds the struct and
/// whatever operator writes.
#[test]
fn writes_through_nested_c_structs_are_refused() {
    let e = err_src(&format!(
        "{GEOMETRY}
         const r0: CGRect = {{ origin: {{ x: 1, y: 2 }}, size: {{ width: 3, height: 4 }} }};
         function main() {{
           const a: CGRect = r0;
           let b = a;
           const xs: CGRect[] = [a];
           a.origin.x = 9;
           b.origin.y += 1;
           r0.size.width = 3;
           xs[0].origin.x = 9;
           xs[0].size.height++;
           b.size = {{ width: 0, height: 0 }};
           console.log(a.origin.x, b.origin.x);
         }}"
    ));
    let n = e.matches("it is a readonly field").count();
    assert_eq!(n, 6, "{e}");
    assert_eq!(e.matches("make a changed copy").count(), 6, "{e}");
}

/// A C struct has no identity, so `==` compares its content in lowering (velt_vir same.rs),
/// through options and tuples too; sema accepts it (the results: veltc tests/c_structs.rs).
#[test]
fn c_structs_compare_by_content() {
    ok_src(&format!(
        "{GEOMETRY}
         function pick(p: CGPoint): CGPoint | null {{ return p.x > 0 ? p : null; }}
         function main() {{
           const p: CGPoint = {{ x: 1, y: 2 }};
           const t: [CGPoint, i32] = [p, 1];
           console.log(p == p, p !== p, pick(p) == pick(p), t == t, pick(p) == null);
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
