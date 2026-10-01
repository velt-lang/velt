//! The WebAssembly targets: VIR's 8-byte pointer slots around 4-byte wasm32 pointers.

use super::*;
use crate::{emit_object, CodegenOptions};

#[test]
fn pointer_slots_stay_eight_bytes_on_wasm32() {
    let ir = emit_ir(&vtable_sample(), "wasm32-wasip1").unwrap();
    for needle in [
        "target triple = \"wasm32-wasip1\"",
        // Every relocated slot: the 32-bit address, then four zero bytes.
        "<{ [8 x i8], ptr, [4 x i8], ptr, [4 x i8], ptr, [4 x i8], [8 x i8] }>",
        "ptr @\"helper\", [4 x i8] zeroinitializer",
        // A `Ptr` local is an i64 slot: stores zero-extend, loads truncate.
        "alloca i64, align 8",
        "ptrtoint ptr %",
        "inttoptr i64 %",
        "attributes #0 = { nounwind }",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
    }
    assert!(!ir.contains("alloca ptr"), "no pointer-sized slots:\n{ir}");
    let native = emit_ir(&vtable_sample(), "x86_64-unknown-linux-gnu").unwrap();
    assert!(native.contains("alloca ptr") && !native.contains("zeroinitializer"));
}

#[test]
fn wasm_objects_when_llc_is_available() {
    if crate::find_wasm_tools().is_none() {
        eprintln!("note: opt/llc not available; skipping wasm object emission");
        return;
    }
    for (target, optimize) in [("wasm32-wasip1", false), ("wasm32-unknown-unknown", true)] {
        let opts = CodegenOptions {
            target: target.into(),
            optimize,
        };
        let obj = emit_object(&sample(), &opts).unwrap();
        assert_eq!(&obj[..4], b"\0asm", "{target}");
    }
}
