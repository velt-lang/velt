//! `load_partial` and `store_partial`: the `count` elements left at the end of an array (fewer
//! than a vector's), in a few scalar loads or stores that stay inside them. With `c` bytes
//! left: 9 to 15 are two overlapping 8-byte accesses, 4 to 8 two overlapping 4-byte ones, 1 to
//! 3 three single bytes (the first, the middle and the last). Each load is shifted into place
//! in a register, so nothing goes through memory twice.

use velt_vir::simd::Lanes;

use super::{element, spell, Body};

/// The vector of the `%a2` elements from `%a1` of the array `%a0`, zeros above them.
pub(super) fn load(b: &mut Body, lanes: Lanes) -> String {
    let (lane, _, _) = spell(lanes);
    let p = element(b, lane, 0);
    let c = b.inst(format!("mul i64 %a2, {}", lanes.bits() / 8));
    let big = b.inst(format!("icmp ugt i64 {c}, 8"));
    b.lines
        .push_str(&format!("  br i1 {big}, label %big, label %notbig\nbig:\n"));
    let lo8 = b.inst(format!("load i64, ptr {p}, align 1"));
    let end = b.inst(format!("getelementptr inbounds i8, ptr {p}, i64 {c}"));
    let q = b.inst(format!("getelementptr inbounds i8, ptr {end}, i64 -8"));
    let hi = b.inst(format!("load i64, ptr {q}, align 1"));
    // The bytes past the first eight are the top `c - 8` of `hi`.
    let missing = b.inst(format!("sub i64 16, {c}"));
    let shift = b.inst(format!("shl i64 {missing}, 3"));
    let hi8 = b.inst(format!("lshr i64 {hi}, {shift}"));
    b.lines.push_str("  br label %join\nnotbig:\n");
    let four = b.inst(format!("icmp uge i64 {c}, 4"));
    b.lines.push_str(&format!(
        "  br i1 {four}, label %four, label %small\nfour:\n"
    ));
    let a = b.inst(format!("load i32, ptr {p}, align 1"));
    let end4 = b.inst(format!("getelementptr inbounds i8, ptr {p}, i64 {c}"));
    let q4 = b.inst(format!("getelementptr inbounds i8, ptr {end4}, i64 -4"));
    let z = b.inst(format!("load i32, ptr {q4}, align 1"));
    let a64 = b.inst(format!("zext i32 {a} to i64"));
    let z64 = b.inst(format!("zext i32 {z} to i64"));
    // Bytes 4 to `c - 1` are the top `c - 4` bytes of `z`.
    let missing4 = b.inst(format!("sub i64 8, {c}"));
    let shift4 = b.inst(format!("shl i64 {missing4}, 3"));
    let top = b.inst(format!("lshr i64 {z64}, {shift4}"));
    let placed = b.inst(format!("shl i64 {top}, 32"));
    let lo4 = b.inst(format!("or i64 {a64}, {placed}"));
    b.lines.push_str("  br label %join\nsmall:\n");
    let none = b.inst(format!("icmp eq i64 {c}, 0"));
    b.lines.push_str(&format!(
        "  br i1 {none}, label %join, label %bytes\nbytes:\n"
    ));
    let half = b.inst(format!("lshr i64 {c}, 1"));
    let last = b.inst(format!("sub i64 {c}, 1"));
    let mut word = String::new();
    for (k, at) in ["0", half.as_str(), last.as_str()].into_iter().enumerate() {
        let ptr = b.inst(format!("getelementptr inbounds i8, ptr {p}, i64 {at}"));
        let byte = b.inst(format!("load i8, ptr {ptr}, align 1"));
        let wide = b.inst(format!("zext i8 {byte} to i64"));
        let bits = b.inst(format!("shl i64 {at}, 3"));
        let moved = b.inst(format!("shl i64 {wide}, {bits}"));
        word = match k {
            0 => moved,
            _ => b.inst(format!("or i64 {word}, {moved}")),
        };
    }
    b.lines.push_str("  br label %join\njoin:\n");
    let low = b.inst(format!(
        "phi i64 [ {lo8}, %big ], [ {lo4}, %four ], [ 0, %small ], [ {word}, %bytes ]"
    ));
    let high = b.inst(format!(
        "phi i64 [ {hi8}, %big ], [ 0, %four ], [ 0, %small ], [ 0, %bytes ]"
    ));
    let v0 = b.inst(format!(
        "insertelement <2 x i64> zeroinitializer, i64 {low}, i64 0"
    ));
    let v1 = b.inst(format!("insertelement <2 x i64> {v0}, i64 {high}, i64 1"));
    b.inst(format!("bitcast <2 x i64> {v1} to <16 x i8>"))
}

/// Writes the low `%a3` lanes of `%a0` to the elements from `%a2` of the array `%a1`.
pub(super) fn store(b: &mut Body, lanes: Lanes) {
    let (lane, _, _) = spell(lanes);
    let words = b.inst("bitcast <16 x i8> %a0 to <2 x i64>".to_string());
    let lo = b.inst(format!("extractelement <2 x i64> {words}, i64 0"));
    let hi = b.inst(format!("extractelement <2 x i64> {words}, i64 1"));
    let p = element(b, lane, 1);
    let c = b.inst(format!("mul i64 %a3, {}", lanes.bits() / 8));
    let big = b.inst(format!("icmp ugt i64 {c}, 8"));
    b.lines
        .push_str(&format!("  br i1 {big}, label %big, label %notbig\nbig:\n"));
    b.lines
        .push_str(&format!("  store i64 {lo}, ptr {p}, align 1\n"));
    // Bytes `c - 8` to `c - 1` of the vector: `k = c - 8` is 1 to 7.
    let k = b.inst(format!("sub i64 {c}, 8"));
    let kbits = b.inst(format!("shl i64 {k}, 3"));
    let down = b.inst(format!("lshr i64 {lo}, {kbits}"));
    let upbits = b.inst(format!("sub i64 64, {kbits}"));
    let up = b.inst(format!("shl i64 {hi}, {upbits}"));
    let tail = b.inst(format!("or i64 {down}, {up}"));
    let q = b.inst(format!("getelementptr inbounds i8, ptr {p}, i64 {k}"));
    b.lines.push_str(&format!(
        "  store i64 {tail}, ptr {q}, align 1\n  br label %done\nnotbig:\n"
    ));
    let four = b.inst(format!("icmp uge i64 {c}, 4"));
    b.lines.push_str(&format!(
        "  br i1 {four}, label %four, label %small\nfour:\n"
    ));
    let a = b.inst(format!("trunc i64 {lo} to i32"));
    b.lines
        .push_str(&format!("  store i32 {a}, ptr {p}, align 1\n"));
    let k4 = b.inst(format!("sub i64 {c}, 4"));
    let k4bits = b.inst(format!("shl i64 {k4}, 3"));
    let down4 = b.inst(format!("lshr i64 {lo}, {k4bits}"));
    let z = b.inst(format!("trunc i64 {down4} to i32"));
    let q4 = b.inst(format!("getelementptr inbounds i8, ptr {p}, i64 {k4}"));
    b.lines.push_str(&format!(
        "  store i32 {z}, ptr {q4}, align 1\n  br label %done\nsmall:\n"
    ));
    let none = b.inst(format!("icmp eq i64 {c}, 0"));
    b.lines.push_str(&format!(
        "  br i1 {none}, label %done, label %bytes\nbytes:\n"
    ));
    let half = b.inst(format!("lshr i64 {c}, 1"));
    let last = b.inst(format!("sub i64 {c}, 1"));
    for at in ["0", half.as_str(), last.as_str()] {
        let bits = b.inst(format!("shl i64 {at}, 3"));
        let moved = b.inst(format!("lshr i64 {lo}, {bits}"));
        let byte = b.inst(format!("trunc i64 {moved} to i8"));
        let ptr = b.inst(format!("getelementptr inbounds i8, ptr {p}, i64 {at}"));
        b.lines
            .push_str(&format!("  store i8 {byte}, ptr {ptr}, align 1\n"));
    }
    b.lines.push_str("  br label %done\ndone:\n");
}
