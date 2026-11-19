//! C structs with pointer fields (#3094): each struct's aggregate is C's layout on the 64-bit
//! targets (lowering checks every one against C's rules, `check_c_struct_layout`), a pointer
//! field one word with `NULL` as `null`; `Ptr.read` and `Ptr.write` load and store the element
//! as it is, checking a read's non-null pointers; and on wasm32, where Velt's pointers and
//! `usize` take 8 bytes and C's 4, they are a compile error for an element holding one.

use super::c_wasi::{check_for, lower_checked_for};
use crate::vir::{self, Ty};

const LINUX: &str = "x86_64-unknown-linux-gnu";
const WASI: &str = "wasm32-wasip1";

/// Structs of each shape, each built and read so that it is laid out.
const SRC: &str = "import { Ptr } from \"velt/ffi\";\n\
    declare c struct P1 { p: Ptr<u8>; }\n\
    declare c struct P2 { a: i8; p: Ptr<u8> | null; }\n\
    declare c struct P3 { p: Ptr<u8>; n: i32; }\n\
    declare c struct P4 { x: f32; p: Ptr<P4> | null; y: f32; }\n\
    declare c struct P5 { b: bool; inner: P2; c: u16; }\n\
    declare c struct Flat { x: f64; y: i32; }\n\
    function at(a: usize): Ptr<u8> {\n  \
    // SAFETY: never read through.\n  \
    return unsafe { Ptr.at<u8>(a) };\n}\n\
    function main() {\n  const p = at(16);\n  \
    const a: P1 = { p };\n  const b: P2 = { a: 1, p: null };\n  const c: P3 = { p, n: 2 };\n  \
    const d: P4 = { x: 1, p: null, y: 2 };\n  const e: P5 = { b: true, inner: b, c: 3 };\n  \
    const f: Flat = { x: 1, y: 2 };\n  \
    console.log(a, b, c, d, e, f);\n  \
    // SAFETY: a test of lowering only; never run.\n  \
    unsafe {\n    Ptr.write(Ptr.at<P5>(64), 1, e);\n    console.log(Ptr.read(Ptr.at<P5>(64), 1), Ptr.read(Ptr.at<Flat>(64), 0), Ptr.read(Ptr.at<f64>(64), 0), Ptr.read(Ptr.at<P3>(64), 2));\n  }\n  ;\n}\n";

fn lowered(target: &str) -> Result<vir::Program, String> {
    let (p, sm) = check_for(SRC, target);
    lower_checked_for(&p, &sm, target)
}

/// `name`'s size, alignment and (field type, offset)s.
fn layout(v: &vir::Program, name: &str) -> (u32, u32, Vec<(Ty, u32)>) {
    let a = (v.aggs.iter())
        .find(|a| a.name == name)
        .unwrap_or_else(|| panic!("no `{name}` layout"));
    (a.size, a.align, a.fields.clone())
}

#[test]
fn pointer_fields_take_c_s_offsets() {
    let v = lowered(LINUX).unwrap_or_else(|e| panic!("lower:\n{e}"));
    let (s, a, f) = layout(&v, "P1");
    assert_eq!((s, a, f), (8, 8, vec![(Ty::Ptr, 0)]), "P1");
    let (s, a, f) = layout(&v, "P2");
    assert_eq!((s, a, f), (16, 8, vec![(Ty::I8, 0), (Ty::Ptr, 8)]), "P2");
    let (s, a, f) = layout(&v, "P3");
    assert_eq!((s, a, f), (16, 8, vec![(Ty::Ptr, 0), (Ty::I32, 8)]), "P3");
    let (s, a, f) = layout(&v, "P4");
    assert_eq!(
        (s, a, f),
        (24, 8, vec![(Ty::F32, 0), (Ty::Ptr, 8), (Ty::F32, 16)]),
        "P4"
    );
    let (s, a, f) = layout(&v, "P5");
    let inner = f[1].0;
    assert!(matches!(inner, Ty::Agg(_)), "P5's inner: {f:?}");
    assert_eq!(
        (s, a, f),
        (32, 8, vec![(Ty::Bool, 0), (inner, 8), (Ty::U16, 24)]),
        "P5"
    );
}

/// The instance of `Ptr.read` or `Ptr.write` (`which`) for element type `elem`.
fn instance(v: &vir::Program, which: &str, elem: &str) -> String {
    let f = (v.funcs.iter())
        .find(|f| {
            f.symbol.contains(&format!("{which}_T")) && f.symbol.ends_with(&format!("_20{elem}"))
        })
        .unwrap_or_else(|| panic!("no `Ptr.{which}<{elem}>`"));
    format!("{f:?}")
}

/// The element is one load or store at `p + index * size` (`P5` is 32 bytes), with no call but
/// a read's stop: `P3`'s `p` is a non-null pointer, compared with 0, and `P5` has none (its
/// `inner.p` may be `null`), so no check.
#[test]
fn read_and_write_move_the_element_with_c_s_stride() {
    let v = lowered(LINUX).unwrap_or_else(|e| panic!("lower:\n{e}"));
    let stride = |n: u32| {
        format!("Binary(Mul, Copy(Place {{ local: Local(1), proj: [] }}), Const(Int({n}), U64))")
    };
    let read5 = instance(&v, "read", "P5");
    let write5 = instance(&v, "write", "P5");
    let read3 = instance(&v, "read", "P3");
    assert!(read5.contains(&stride(32)), "{read5}");
    assert!(write5.contains(&stride(32)), "{write5}");
    assert!(read3.contains(&stride(16)), "{read3}");
    // The check: `p` against 0, a branch to the stop (a call of the runtime's panic).
    assert!(
        read3.contains("Const(Int(0), U64)))], term: Branch")
            && read3.contains("term: Unreachable"),
        "no check of `P3.p`: {read3}"
    );
    for (f, body) in [("read<P5>", &read5), ("write<P5>", &write5)] {
        assert!(
            !body.contains("term: Branch"),
            "{f} checks a pointer: {body}"
        );
        assert!(!body.contains("term: Call"), "{f} calls: {body}");
    }
}

/// On wasm32, a struct with a pointer field (`P5`, through `P2`) has another layout in C, so
/// `Ptr.read` and `Ptr.write` of it are errors at the user's call; one of numbers only (`Flat`)
/// and a number are not.
#[test]
fn wasm32_refuses_an_element_whose_layout_is_not_c_s() {
    let e = lowered(WASI)
        .err()
        .unwrap_or_else(|| panic!("lowered for {WASI}"));
    for want in [
        "`Ptr.read` of a `P5` is not available on wasm32-wasip1",
        "`Ptr.write` of a `P5` is not available on wasm32-wasip1",
        "`Ptr.read` of a `P3` is not available on wasm32-wasip1",
        "take 8 bytes in memory, and C's 4",
    ] {
        assert!(e.contains(want), "missing `{want}`:\n{e}");
    }
    for not in ["of a `Flat`", "of a `f64`"] {
        assert!(!e.contains(not), "refused {not}:\n{e}");
    }
}
