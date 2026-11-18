//! 256-bit vectors (`velt_vir::simd::wide`, #3140) in a function: a value of two 128-bit halves
//! is written with one store of all 32 bytes, and its operations are calls of the inline helpers
//! with the values' addresses.

use super::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::*;

/// `velt_main`: `v = { 0, 0 }`, then `velt_simd_f32x8_add(&v, &v, &w)`; and a pair of `u64`s
/// built the same way.
fn sample() -> Program {
    let mut pb = ProgramBuilder::new();
    let wide = pb.agg("f32x8", 32, 16, &[(V128, 0), (V128, 16)]);
    let pair = pb.agg("pair", 16, 8, &[(U64, 0), (U64, 8)]);
    let add = pb.ext("velt_simd_f32x8_add", &[Ptr, Ptr, Ptr], Unit, false);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let (v, w, p) = (
        fb.local(Agg(wide)),
        fb.local(Agg(wide)),
        fb.local(Agg(pair)),
    );
    let (pv, pw) = (fb.local(Ptr), fb.local(Ptr));
    let zero = Operand::Const(Const::Int(0), V128);
    let b = fb.block();
    fb.assign(b, v, Rvalue::Aggregate(wide, vec![zero.clone(), zero]));
    fb.assign(
        b,
        p,
        Rvalue::Aggregate(pair, vec![int(1, U64), int(2, U64)]),
    );
    fb.assign(b, pv, Rvalue::AddrOf(Place::local(v)));
    fb.assign(b, pw, Rvalue::AddrOf(Place::local(w)));
    let args = vec![copy_local(pv), copy_local(pv), copy_local(pw)];
    let b = fb.call(b, Callee::Extern(add), args, None);
    fb.ret(b, int(0, I32));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn a_wide_vector_is_written_with_one_store() {
    let ir = crate::emit_ir(&sample(), "x86_64-unknown-linux-gnu").expect("emit_ir");
    // Both halves at once: a store of each would have LLVM split the slot, and every value
    // copied to or from it, into two 128-bit ones.
    assert_eq!(ir.matches("store <32 x i8>").count(), 1, "{ir}");
    assert!(
        ir.contains("shufflevector <16 x i8>") && !ir.contains("store <16 x i8>"),
        "{ir}"
    );
    // Any other aggregate is still written field by field.
    assert_eq!(ir.matches("store i64").count(), 2, "{ir}");
    // The operation is the inline helper, on the addresses.
    assert!(
        ir.contains(
            "define internal void @velt.simd.f32x8_add(ptr %a0, ptr %a1, ptr %a2) alwaysinline"
        ),
        "{ir}"
    );
    assert!(ir.contains("call void @velt.simd.f32x8_add(ptr "), "{ir}");
    assert!(ir.contains("fadd <8 x float>"), "{ir}");
}
