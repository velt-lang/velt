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

/// A program that uses vectors asks for WebAssembly's SIMD instructions in every function (a
/// vector is passed differently with and without them); other programs, and other targets,
/// do not.
#[test]
fn programs_with_vectors_ask_for_simd128() {
    let with_vectors = || {
        let mut pb = ProgramBuilder::new();
        let add = pb.ext("velt_simd_u8x16_add", &[V128, V128], V128, false);
        let mut helper = FuncBuilder::internal("plain", &[], I32);
        let b = helper.block();
        helper.ret(b, int(1, I32));
        pb.add(helper.finish());
        let mut fb = FuncBuilder::export("velt_main", &[], I32);
        let b = fb.block();
        let (x, sum) = (fb.local(V128), fb.local(V128));
        fb.assign(b, x, Rvalue::Use(int(0, V128)));
        let args = vec![copy_local(x), copy_local(x)];
        let b = fb.call(b, Callee::Extern(add), args, Some(sum));
        fb.ret(b, int(0, I32));
        pb.add(fb.finish());
        pb.finish()
    };
    let feature = "\"target-features\"=\"+simd128\"";
    let ir = emit_ir(&with_vectors(), "wasm32-wasip1").unwrap();
    let defined = ir.lines().filter(|l| l.starts_with("define")).count();
    let asking = ir.lines().filter(|l| l.contains(feature)).count();
    assert_eq!((defined, asking), (3, 3), "{ir}");
    assert!(!emit_ir(&sample(), "wasm32-wasip1")
        .unwrap()
        .contains(feature));
    let native = emit_ir(&with_vectors(), "aarch64-apple-darwin").unwrap();
    assert!(!native.contains(feature), "{native}");
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
