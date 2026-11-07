//! `SimdCall::of_symbol` and the signatures of the SIMD externs.

use super::*;

/// The storage-float vectors: what they have, what they do not, and the operations on
/// `f32x4` that take them.
#[test]
fn half_lanes() {
    use Ty::{Ptr, U16, U64, V128};
    let of = |s: &str| SimdCall::of_symbol(s);
    let sig = |s: &str| of(s).map(SimdCall::signature);
    assert_eq!(Lanes::F16.name(), "f16x8");
    assert_eq!(Lanes::BF16.name(), "bf16x8");
    assert_eq!((Lanes::F16.count(), Lanes::BF16.bits()), (8, 16));
    assert_eq!(sig("velt_simd_f16x8_splat"), Some((vec![U16], V128)));
    assert_eq!(sig("velt_simd_bf16x8_lane"), Some((vec![V128, U64], U16)));
    assert_eq!(sig("velt_simd_bf16x8_load"), Some((vec![Ptr, U64], V128)));
    assert_eq!(
        of("velt_simd_f16x8_as_u16x8"),
        Some(SimdCall::Vector(Lanes::F16, Op::As(Lanes::U16)))
    );
    assert_eq!(
        of("velt_simd_u16x8_as_bf16x8"),
        Some(SimdCall::Vector(Lanes::U16, Op::As(Lanes::BF16)))
    );
    assert_eq!(
        of("velt_simd_bf16x8_widen_high"),
        Some(SimdCall::Vector(Lanes::BF16, Op::WidenHigh))
    );
    assert_eq!(
        of("velt_simd_f32x4_narrow_f16x8"),
        Some(SimdCall::Vector(Lanes::F32, Op::NarrowTo(Lanes::F16)))
    );
    assert_eq!(
        of("velt_simd_f32x4_dot_add_f16x8"),
        Some(SimdCall::Vector(Lanes::F32, Op::DotAddHalf(Lanes::F16)))
    );
    assert_eq!(
        of("velt_simd_f32x4_dot_add_fast_bf16x8"),
        Some(SimdCall::Vector(Lanes::F32, Op::DotAddFast(Lanes::BF16)))
    );
    assert_eq!(
        sig("velt_simd_f32x4_dot_add_bf16x8"),
        Some((vec![V128, V128, V128], V128))
    );
    assert_eq!(
        sig("velt_simd_f32x4_narrow_bf16x8"),
        Some((vec![V128, V128], V128))
    );
    // Moved and reinterpreted, not computed on: every other operation is left out by name, so
    // that one `velt_opt` writes on the bits and the array (`fits_both`) exists here too.
    for lanes in ["f16x8", "bf16x8"] {
        for op in [
            "splat",
            "fits",
            "fits_both",
            "load",
            "store",
            "load_partial",
            "store_partial",
            "lane",
            "with_lane",
            "as_u16x8",
            "shuffle_0_1_2_3_8_9_10_11",
            "widen_low",
            "widen_high",
        ] {
            assert!(of(&format!("velt_simd_{lanes}_{op}")).is_some(), "{op}");
        }
        for op in [
            "index",
            "load_optimistic",
            "add_sat",
            "sub_sat",
            "and",
            "or",
            "xor",
            "not",
            "shl",
            "shr",
            "sum",
            "sum_wide",
            "dot_add",
            "dot_add_f16x8",
            "dot_add_fast_bf16x8",
            "zip_low",
            "zip_high",
            "narrow",
            "narrow_f16x8",
            "reverse_bytes",
            "to_f32x4",
        ] {
            assert_eq!(of(&format!("velt_simd_{lanes}_{op}")), None, "{op}");
        }
    }
    // `f16x8` computes on its lanes; `bf16x8` has no such instruction to reach, and widens.
    for (name, op) in [
        ("add", Op::Add),
        ("sub", Op::Sub),
        ("mul", Op::Mul),
        ("div", Op::Div),
        ("neg", Op::Neg),
        ("abs", Op::Abs),
        ("sqrt", Op::Sqrt),
        ("mul_add", Op::MulAdd),
        ("mul_add_fast", Op::MulAddFast),
        ("min", Op::Min),
        ("max", Op::Max),
        ("eq", Op::Eq),
        ("ne", Op::Ne),
        ("lt", Op::Lt),
        ("le", Op::Le),
        ("gt", Op::Gt),
        ("ge", Op::Ge),
        ("select", Op::Select),
    ] {
        let symbol = format!("velt_simd_f16x8_{name}");
        assert_eq!(
            of(&symbol),
            Some(SimdCall::Vector(Lanes::F16, op)),
            "{name}"
        );
        assert_eq!(op.is_f16_arithmetic(), op != Op::Select, "{name}");
        assert_eq!(of(&format!("velt_simd_bf16x8_{name}")), None, "{name}");
    }
    assert_eq!(sig("velt_simd_f16x8_lt"), Some((vec![V128, V128], V128)));
    assert_eq!(
        sig("velt_simd_f16x8_select"),
        Some((vec![V128, V128, V128], V128))
    );
    for missing in [
        // `f32x4` only, of storage floats only; no fast f16 form, `fmlal` is exact.
        "velt_simd_f64x2_narrow_f16x8",
        "velt_simd_f32x4_narrow_u16x8",
        "velt_simd_f32x4_dot_add_i8x16",
        "velt_simd_f64x2_dot_add_f16x8",
        "velt_simd_f32x4_dot_add_fast_f16x8",
        "velt_simd_i32x4_dot_add_f16x8",
    ] {
        assert_eq!(of(missing), None, "{missing}");
    }
    assert_eq!(Lanes::F32.narrower(), None);
    assert_eq!(Lanes::U16.narrower(), Some(Lanes::U8));
}

#[test]
fn sha_symbols_and_signatures() {
    use Ty::{Bool, U32, V128};
    for op in ExtOp::ALL {
        let symbol = format!("velt_simd_{}_{}", op.type_name(), op.name());
        assert_eq!(
            SimdCall::of_symbol(&symbol),
            Some(SimdCall::Inst(op)),
            "{symbol}"
        );
    }
    let sig = |s: &str| SimdCall::of_symbol(s).unwrap().signature();
    assert_eq!(sig("velt_simd_u32x4_sha1c"), (vec![V128, U32, V128], V128));
    assert_eq!(sig("velt_simd_u32x4_sha1h"), (vec![U32], U32));
    assert_eq!(
        sig("velt_simd_u32x4_sha256rnds2"),
        (vec![V128, V128, V128], V128)
    );
    assert_eq!(sig("velt_simd_u32x4_sha1rnds4_3"), (vec![V128, V128], V128));
    assert_eq!(sig("velt_simd_has_sha_x86"), (vec![], Bool));
    assert_eq!(SimdCall::of_symbol("velt_simd_u32x4_sha1rnds4_4"), None);
    assert_eq!(SimdCall::of_symbol("velt_simd_has_sha_mips"), None);
    assert_eq!(ExtOp::Sha256Msg2.ext(), Ext::ShaX86);
    assert_eq!(ExtOp::Sha1H.ext(), Ext::ShaArm);
    assert_eq!(
        sig("velt_simd_u64x2_pclmulqdq_16"),
        (vec![V128, V128], V128)
    );
    assert_eq!(SimdCall::of_symbol("velt_simd_u64x2_pclmulqdq_2"), None);
    assert_eq!(SimdCall::of_symbol("velt_simd_u32x4_pmull_0"), None);
    assert_eq!(ExtOp::Pmull(17).ext(), Ext::ClmulArm);
    assert_eq!(ExtOp::Pclmulqdq(1).ext(), Ext::ClmulX86);
    assert_eq!(sig("velt_simd_u8x16_aesenc"), (vec![V128, V128], V128));
    assert_eq!(sig("velt_simd_u8x16_aesimc"), (vec![V128], V128));
    assert_eq!(sig("velt_simd_u8x16_aesimc_arm"), (vec![V128], V128));
    assert_eq!(sig("velt_simd_u8x16_aeskeygenassist"), (vec![V128], V128));
    assert_eq!(sig("velt_simd_has_aes_arm"), (vec![], Bool));
    assert_eq!(sig("velt_simd_cpuid"), (vec![U32, U32], V128));
    assert_eq!(ExtOp::AesimcArm.ext(), Ext::AesArm);
    assert_eq!(ExtOp::Aeskeygenassist.ext(), Ext::AesX86);
    assert_eq!(SimdCall::of_symbol("velt_simd_u32x4_aesenc"), None);
    assert!(Ext::AesX86.covers(Ext::ClmulX86) && !Ext::ClmulX86.covers(Ext::AesX86));
    assert!(Ext::AesArm.covers(Ext::ClmulArm) && !Ext::ClmulArm.covers(Ext::AesArm));
    assert!(!Ext::AesArm.covers(Ext::ClmulX86) && !Ext::AesX86.covers(Ext::ClmulArm));
}

#[test]
fn shuffles_spell_lanes_and_mean_bytes() {
    let call = SimdCall::of_symbol("velt_simd_u32x4_shuffle_1_0_7_6").unwrap();
    let mut mask = [0u8; 16];
    for (lane, from) in [1u8, 0, 7, 6].into_iter().enumerate() {
        for byte in 0..4 {
            mask[lane * 4 + byte] = from * 4 + byte as u8;
        }
    }
    assert_eq!(call, SimdCall::Vector(Lanes::U32, Op::Shuffle(mask)));
    assert_eq!(
        shuffle_symbol(Lanes::U32, &[1, 0, 7, 6]),
        "velt_simd_u32x4_shuffle_1_0_7_6"
    );
    // Wrong count, out of range, not a number.
    assert_eq!(SimdCall::of_symbol("velt_simd_u32x4_shuffle_1_0_7"), None);
    assert_eq!(SimdCall::of_symbol("velt_simd_u32x4_shuffle_1_0_7_8"), None);
    assert_eq!(SimdCall::of_symbol("velt_simd_u32x4_shuffle_1_0_x_6"), None);
    assert_eq!(
        SimdCall::of_symbol("velt_simd_u32x4_reverse_bytes"),
        Some(SimdCall::Vector(Lanes::U32, Op::ReverseBytes))
    );
    assert_eq!(SimdCall::of_symbol("velt_simd_u8x16_reverse_bytes"), None);
    assert_eq!(SimdCall::of_symbol("velt_simd_f32x4_reverse_bytes"), None);
}

#[test]
fn symbols() {
    assert_eq!(
        SimdCall::of_symbol("velt_simd_u8x16_eq"),
        Some(SimdCall::Vector(Lanes::U8, Op::Eq))
    );
    assert_eq!(
        SimdCall::of_symbol("velt_simd_f32x4_as_u32x4"),
        Some(SimdCall::Vector(Lanes::F32, Op::As(Lanes::U32)))
    );
    assert_eq!(
        SimdCall::of_symbol("velt_simd_i16x8_with_lane"),
        Some(SimdCall::Vector(Lanes::I16, Op::WithLane))
    );
    assert_eq!(
        SimdCall::of_symbol("velt_simd_mask32x4_first"),
        Some(SimdCall::Mask(32, MaskOp::First))
    );
    assert_eq!(
        SimdCall::of_symbol("velt_simd_u32x4_to_f32x4"),
        Some(SimdCall::Vector(Lanes::U32, Op::Convert(Lanes::F32)))
    );
    assert_eq!(
        SimdCall::of_symbol("velt_simd_i16x8_widen_high"),
        Some(SimdCall::Vector(Lanes::I16, Op::WidenHigh))
    );
    assert_eq!(
        SimdCall::of_symbol("velt_simd_i32x4_dot_add"),
        Some(SimdCall::Vector(Lanes::I32, Op::DotAdd))
    );
    assert_eq!(SimdCall::of_symbol("velt_simd_u32x4_dot_add"), None);
    let sig = |s: &str| SimdCall::of_symbol(s).map(SimdCall::signature);
    assert_eq!(sig("velt_simd_i8x16_sum"), Some((vec![Ty::V128], Ty::I8)));
    assert_eq!(sig("velt_simd_u64x2_sum"), Some((vec![Ty::V128], Ty::U64)));
    assert_eq!(
        sig("velt_simd_i8x16_sum_wide"),
        Some((vec![Ty::V128], Ty::I32))
    );
    assert_eq!(
        sig("velt_simd_u16x8_sum_wide"),
        Some((vec![Ty::V128], Ty::U32))
    );
    assert_eq!(
        sig("velt_simd_i32x4_sum_wide"),
        Some((vec![Ty::V128], Ty::I64))
    );
    assert_eq!(
        sig("velt_simd_u32x4_sum_wide"),
        Some((vec![Ty::V128], Ty::U64))
    );
    for missing in [
        "velt_simd_i64x2_sum_wide",
        "velt_simd_f32x4_sum",
        "velt_simd_f64x2_sum_wide",
    ] {
        assert_eq!(SimdCall::of_symbol(missing), None, "{missing}");
    }
    assert_eq!(SimdCall::of_symbol("velt_simd_i16x8_dot_add"), None);
    assert_eq!(
        SimdCall::of_symbol("velt_simd_f64x2_mul_add"),
        Some(SimdCall::Vector(Lanes::F64, Op::MulAdd))
    );
    assert_eq!(
        SimdCall::of_symbol("velt_simd_f32x4_mul_add_fast"),
        Some(SimdCall::Vector(Lanes::F32, Op::MulAddFast))
    );
    for unsupported in [
        "velt_simd_i32x4_mul_add",
        "velt_simd_u16x8_mul_add_fast",
        "velt_simd_i64x2_widen_low",
        "velt_simd_u8x16_narrow",
        "velt_simd_f32x4_to_f64x2",
        "velt_simd_i16x8_to_f32x4",
        "velt_simd_f32x4_widen_low",
        "velt_simd_u8x16_mul",
        "velt_simd_i32x4_div",
        "velt_simd_f64x2_and",
        "velt_simd_u32x4_neg",
        "velt_simd_i64x2_min",
        "velt_simd_i32x4_add_sat",
        "velt_simd_u8x32_add",
        "velt_rt_math_sqrt",
    ] {
        assert_eq!(SimdCall::of_symbol(unsupported), None, "{unsupported}");
    }
}

/// Every `declare function velt_simd_*` of the standard library names an operation, with
/// as many parameters as the operation takes (the verifier checks their types whenever a
/// program uses one).
#[test]
fn std_declarations_are_operations() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std/simd");
    let mut seen = 0;
    for entry in std::fs::read_dir(dir).expect("std/simd") {
        let text = std::fs::read_to_string(entry.expect("entry").path()).expect("source");
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("declare function velt_simd_") else {
                continue;
            };
            let (name, params) = rest.split_once('(').expect("parameter list");
            let symbol = format!("velt_simd_{name}");
            let call = SimdCall::of_symbol(&symbol)
                .unwrap_or_else(|| panic!("{symbol} is not a SIMD operation"));
            let list = params.split_once(')').expect("`)`").0;
            let declared = list.split(", ").filter(|p| !p.is_empty()).count();
            assert_eq!(call.signature().0.len(), declared, "{symbol}");
            seen += 1;
        }
    }
    assert!(seen > 300, "found only {seen} SIMD declarations");
}

#[test]
fn signatures() {
    let sig = |s: &str| SimdCall::of_symbol(s).unwrap().signature();
    assert_eq!(sig("velt_simd_i8x16_splat"), (vec![Ty::I8], Ty::V128));
    assert_eq!(
        sig("velt_simd_f32x4_store"),
        (vec![Ty::V128, Ty::Ptr, Ty::U64], Ty::Unit)
    );
    assert_eq!(
        sig("velt_simd_u16x8_lane"),
        (vec![Ty::V128, Ty::U64], Ty::U16)
    );
    assert_eq!(sig("velt_simd_mask8x16_any"), (vec![Ty::V128], Ty::Bool));
    assert_eq!(
        sig("velt_simd_u32x4_shr"),
        (vec![Ty::V128, Ty::U32], Ty::V128)
    );
}
