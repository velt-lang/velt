//! The helpers on masks: one lane per vector lane, all bits set or none.

use velt_vir::simd::MaskOp;

use super::{mask_type, not, Body};

/// The body of a mask operation; the value to return.
pub(super) fn mask(b: &mut Body, bits: u32, op: MaskOp, neon: bool) -> Option<String> {
    let bytes = "<16 x i8>";
    let bitwise = match op {
        MaskOp::And => Some("and"),
        MaskOp::Or => Some("or"),
        MaskOp::Xor => Some("xor"),
        _ => None,
    };
    if let Some(instruction) = bitwise {
        return Some(b.inst(format!("{instruction} {bytes} %a0, %a1")));
    }
    if op == MaskOp::Not {
        return Some(not(b, bytes, 16, "%a0"));
    }
    let n = 128 / bits;
    let ty = mask_type(bits);
    let x = b.arg(0, &ty);
    let set = b.inst(format!("icmp slt {ty} {x}, zeroinitializer"));
    let word = b.inst(format!("bitcast <{n} x i1> {set} to i{n}"));
    Some(match op {
        MaskOp::Any | MaskOp::All => {
            let test = if op == MaskOp::Any {
                format!("icmp ne i{n} {word}, 0")
            } else {
                format!("icmp eq i{n} {word}, -1")
            };
            let c = b.inst(test);
            b.inst(format!("zext i1 {c} to i8"))
        }
        MaskOp::Bitmask => b.inst(format!("zext i{n} {word} to i64")),
        MaskOp::Count if neon => return Some(count_neon(b, bits)),
        MaskOp::Count => {
            let wide = b.inst(format!("zext i{n} {word} to i64"));
            b.declarations
                .push("declare i64 @llvm.ctpop.i64(i64)".to_string());
            b.inst(format!("call i64 @llvm.ctpop.i64(i64 {wide})"))
        }
        _ => {
            let wide = b.inst(format!("zext i{n} {word} to i64"));
            let stop = b.inst(format!("or i64 {wide}, {}", 1u64 << n));
            b.declarations
                .push("declare i64 @llvm.cttz.i64(i64, i1)".to_string());
            b.inst(format!("call i64 @llvm.cttz.i64(i64 {stop}, i1 true)"))
        }
    })
}

/// The number of set lanes of the mask `%a0` on NEON: the top bit of every byte, summed by
/// `uaddlv`, is the lane count times the bytes per lane. LLVM turns the generic forms (the
/// bits of the bitmask, a sum of ones) into a count of bitmask bits, which NEON has no
/// instruction for.
fn count_neon(b: &mut Body, bits: u32) -> String {
    let shift = vec!["i8 7"; 16].join(", ");
    let top = b.inst(format!("lshr <16 x i8> %a0, <{shift}>"));
    b.declarations
        .push("declare i32 @llvm.aarch64.neon.uaddlv.i32.v16i8(<16 x i8>)".to_string());
    let sum = b.inst(format!(
        "call i32 @llvm.aarch64.neon.uaddlv.i32.v16i8(<16 x i8> {top})"
    ));
    let lanes = b.inst(format!("lshr i32 {sum}, {}", (bits / 8).trailing_zeros()));
    b.inst(format!("zext i32 {lanes} to i64"))
}
