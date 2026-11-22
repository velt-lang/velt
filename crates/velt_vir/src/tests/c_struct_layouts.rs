//! C structs with pointer fields (#3094 "the layout guarantee"): each aggregate is C's layout on
//! the 64-bit targets, a pointer field one word whether it may be `NULL` or not, and
//! `Ptr.read` checks the fields declared non-null.

use super::c_wasi::{check_for, lower_checked_for};
use crate::vir::{self, Ty};

const LINUX: &str = "x86_64-unknown-linux-gnu";

/// One struct per shape, each read through a pointer.
const SRC: &str = "import { Ptr } from \"velt/ffi\";\n\
    declare c struct One { p: Ptr<u8>; }\n\
    declare c struct BytePtr { b: i8; p: Ptr<u8> | null; }\n\
    declare c struct PtrInt { p: Ptr<u8>; n: i32; }\n\
    declare c struct Sandwich { a: f32; p: Ptr<Sandwich> | null; b: f32; }\n\
    declare c struct Outer { tag: u16; inner: PtrInt; last: bool; }\n\
    function main() {\n  \
    // SAFETY: a test of lowering only; never run.\n  \
    const a = unsafe { Ptr.read(Ptr.at<One>(64), 0) };\n  \
    // SAFETY: as above.\n  \
    const b = unsafe { Ptr.read(Ptr.at<BytePtr>(64), 1) };\n  \
    // SAFETY: as above.\n  \
    const c = unsafe { Ptr.read(Ptr.at<Sandwich>(64), 2) };\n  \
    // SAFETY: as above.\n  \
    const d = unsafe { Ptr.read(Ptr.at<Outer>(64), 3) };\n  \
    console.log(a.p, b.b, c.a, d.inner.n);\n}\n";

fn lowered() -> vir::Program {
    let (p, sm) = check_for(SRC, LINUX);
    lower_checked_for(&p, &sm, LINUX).unwrap_or_else(|e| panic!("lower:\n{e}"))
}

/// The aggregate named `name`: its size, alignment and fields (VIR type, offset).
fn layout(v: &vir::Program, name: &str) -> (u32, u32, Vec<(Ty, u32)>) {
    let a = (v.aggs.iter())
        .find(|a| a.name == name)
        .unwrap_or_else(|| panic!("no `{name}` layout"));
    (a.size, a.align, a.fields.clone())
}

#[test]
fn a_pointer_field_is_one_word_at_c_s_offset() {
    let v = lowered();
    assert_eq!(layout(&v, "One"), (8, 8, vec![(Ty::Ptr, 0)]));
    assert_eq!(
        layout(&v, "BytePtr"),
        (16, 8, vec![(Ty::I8, 0), (Ty::Ptr, 8)])
    );
    assert_eq!(
        layout(&v, "PtrInt"),
        (16, 8, vec![(Ty::Ptr, 0), (Ty::I32, 8)])
    );
    assert_eq!(
        layout(&v, "Sandwich"),
        (24, 8, vec![(Ty::F32, 0), (Ty::Ptr, 8), (Ty::F32, 16)])
    );
}

#[test]
fn a_nested_c_struct_is_held_inline_at_c_s_offset() {
    let v = lowered();
    let (size, align, fields) = layout(&v, "Outer");
    assert_eq!((size, align), (32, 8), "{fields:?}");
    assert_eq!(fields[0], (Ty::U16, 0));
    assert!(matches!(fields[1], (Ty::Agg(_), 8)), "{fields:?}");
    assert_eq!(fields[2], (Ty::Bool, 24));
}

/// The stops `Ptr.read` emits: one per field declared non-null, none for `Ptr<T> | null`.
#[test]
fn ptr_read_checks_the_non_null_fields_only() {
    let v = lowered();
    let text = format!("{v}");
    for field in [
        "field `p` of C struct `One`",
        "field `inner.p` of C struct `Outer`",
    ] {
        assert!(text.contains(field), "no stop for {field}");
    }
    for s in ["`BytePtr`", "`Sandwich`"] {
        assert!(
            !text.contains(&format!("of C struct {s}")),
            "a stop for {s}"
        );
    }
}
