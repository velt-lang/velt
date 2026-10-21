//! Alias tags on array accesses (tbaa.rs): which loads and stores get them, and which stay
//! untagged.

use super::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::*;

use crate::emit_ir;

const ELEMENT: &str = "!\"velt array element\"";
const HEADER: &str = "!\"velt array header\"";

/// `f(arr: Ptr, other: Ptr, i: U64) -> U64` storing the bytes 1 to 5 through pointers of each
/// kind, and returning the array's length.
fn program() -> Program {
    let mut pb = ProgramBuilder::new();
    let array = pb.agg(
        velt_vir::ARRAY_AGG_NAME,
        24,
        8,
        &[(Ptr, 0), (U64, 8), (U64, 16)],
    );
    let get = pb.ext("get_ptr", &[], Ptr, false);
    let mut fb = FuncBuilder::internal("f", &[Ptr, Ptr, U64], U64);
    let (arr, other, i) = (fb.param(0), fb.param(1), fb.param(2));
    let data = Place {
        local: arr,
        proj: vec![Proj::Deref(Agg(array)), Proj::Field(0)],
    };
    let len = Place {
        local: arr,
        proj: vec![Proj::Deref(Agg(array)), Proj::Field(1)],
    };
    let b = fb.block();
    let (elem, copied, stepped, exposed, slot, called, n) = (
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(U64),
    );
    // An element pointer, a copy of it and one further on: element tags.
    fb.assign(b, elem, bin(BinOp::PtrAdd, copy_place(data.clone()), copy_local(i)));
    fb.assign(b, copied, Rvalue::Use(copy_local(elem)));
    fb.assign(b, stepped, bin(BinOp::PtrAdd, copy_local(copied), int(1, U64)));
    fb.push(b, Stmt::Assign(deref(copied, U8), Rvalue::Use(int(1, U8))));
    fb.push(b, Stmt::Assign(deref(stepped, U8), Rvalue::Use(int(2, U8))));
    // The same pointer in a local whose address is taken: it could be changed through `slot`.
    fb.assign(b, exposed, bin(BinOp::PtrAdd, copy_place(data), copy_local(i)));
    fb.assign(b, slot, Rvalue::AddrOf(Place::local(exposed)));
    fb.push(b, Stmt::Assign(deref(exposed, U8), Rvalue::Use(int(3, U8))));
    // A pointer from a call and a pointer param: anything.
    let b = fb.call(b, Callee::Extern(get), vec![], Some(called));
    fb.push(b, Stmt::Assign(deref(called, U8), Rvalue::Use(int(4, U8))));
    fb.push(b, Stmt::Assign(deref(other, U8), Rvalue::Use(int(5, U8))));
    // A header field.
    fb.assign(b, n, Rvalue::Use(copy_place(len)));
    fb.ret(b, copy_local(n));
    pb.add(fb.finish());
    let mut main = FuncBuilder::export("velt_main", &[], I32);
    let b = main.block();
    main.ret(b, int(0, I32));
    pb.add(main.finish());
    pb.finish()
}

/// The line of `ir` that stores the byte `v`.
fn store_of(ir: &str, v: u8) -> &str {
    let needle = format!("store i8 {v}, ");
    ir.lines()
        .find(|l| l.contains(&needle))
        .unwrap_or_else(|| panic!("no `{needle}` in\n{ir}"))
}

#[test]
fn element_and_header_accesses_are_tagged_and_nothing_else() {
    let ir = emit_ir(&program(), "aarch64-unknown-linux-gnu").expect("emit_ir");
    for v in [1, 2] {
        let line = store_of(&ir, v);
        assert!(line.contains(ELEMENT), "store {v} should have the element tag: {line}");
    }
    for v in [3, 4, 5] {
        let line = store_of(&ir, v);
        assert!(!line.contains("!tbaa"), "store {v} should have no tag: {line}");
    }
    let header: Vec<&str> = ir.lines().filter(|l| l.contains(HEADER)).collect();
    assert!(
        header.iter().any(|l| l.contains("load i64")),
        "the length load should have the header tag:\n{ir}"
    );
    assert!(
        header.iter().all(|l| l.contains("load")),
        "only loads of header fields are tagged here:\n{header:?}"
    );
}

#[test]
fn programs_without_arrays_get_no_tags() {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::internal("f", &[Ptr, U64], Unit);
    let (p, i) = (fb.param(0), fb.param(1));
    let b = fb.block();
    let q = fb.local(Ptr);
    fb.assign(b, q, bin(BinOp::PtrAdd, copy_local(p), copy_local(i)));
    fb.push(b, Stmt::Assign(deref(q, U8), Rvalue::Use(int(1, U8))));
    fb.ret(b, Operand::Const(Const::Unit, Unit));
    pb.add(fb.finish());
    let mut main = FuncBuilder::export("velt_main", &[], I32);
    let b = main.block();
    main.ret(b, int(0, I32));
    pb.add(main.finish());
    let ir = emit_ir(&pb.finish(), "aarch64-unknown-linux-gnu").expect("emit_ir");
    assert!(!ir.contains("!tbaa"), "{ir}");
}
