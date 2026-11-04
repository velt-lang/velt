//! The helpers of `f16x8` and `bf16x8`: widening to and narrowing from `f32x4`, one instruction
//! per half where the target has it (`fcvtl`, `vcvtph2ps`) and otherwise the sequences of
//! `crate::half` applied to four lanes at a time.

use velt_vir::simd::Lanes;

use super::Body;
use crate::half::{HalfNative, BF16_TO_F32, F16_TO_F32_SOFT, F32_TO_BF16, F32_TO_F16_SOFT};

const FLOATS: &str = "<4 x float>";

/// Whether `lanes` convert to and from `f32` with an instruction: f16 where the target has one.
/// bf16 always takes the sequences (`crate::half`).
fn native(lanes: Lanes, half: HalfNative) -> bool {
    lanes == Lanes::F16 && half != HalfNative::None
}

/// The low (or high) four lanes of the `<8 x i16>` value `x` as `<4 x float>`, exactly.
fn widened(b: &mut Body, x: &str, lanes: Lanes, high: bool, half: HalfNative) -> String {
    let picks = if high {
        "4, i32 5, i32 6, i32 7"
    } else {
        "0, i32 1, i32 2, i32 3"
    };
    let part = b.inst(format!(
        "shufflevector <8 x i16> {x}, <8 x i16> poison, <4 x i32> <i32 {picks}>"
    ));
    if native(lanes, half) {
        let h = b.inst(format!("bitcast <4 x i16> {part} to <4 x half>"));
        return b.inst(format!("fpext <4 x half> {h} to {FLOATS}"));
    }
    let scalar = if lanes == Lanes::F16 {
        F16_TO_F32_SOFT
    } else {
        BF16_TO_F32
    };
    let (name, definition) = four_lanes(scalar);
    b.declarations.push(definition);
    b.inst(format!("call {FLOATS} {name}(<4 x i16> {part})"))
}

/// The four lanes of the `<4 x float>` value `x` rounded to `<4 x i16>` of `to`.
fn narrowed(b: &mut Body, x: &str, to: Lanes, half: HalfNative) -> String {
    if native(to, half) && half == HalfNative::All {
        let h = b.inst(format!("fptrunc {FLOATS} {x} to <4 x half>"));
        return b.inst(format!("bitcast <4 x half> {h} to <4 x i16>"));
    }
    if native(to, half) {
        // Immediate 0: to nearest even, whatever MXCSR says, as `narrow` documents and as every
        // other path rounds (bit 2 set, as in 4, would take the rounding from MXCSR instead).
        let name = "llvm.x86.vcvtps2ph.128";
        b.declarations
            .push(format!("declare <8 x i16> @{name}({FLOATS}, i32)"));
        let all = b.inst(format!("call <8 x i16> @{name}({FLOATS} {x}, i32 0)"));
        return b.inst(format!(
            "shufflevector <8 x i16> {all}, <8 x i16> poison, <4 x i32> <i32 0, i32 1, i32 2, i32 3>"
        ));
    }
    let scalar = if to == Lanes::F16 {
        F32_TO_F16_SOFT
    } else {
        F32_TO_BF16
    };
    let (name, definition) = four_lanes(scalar);
    b.declarations.push(definition);
    b.inst(format!("call <4 x i16> {name}({FLOATS} {x})"))
}

/// `Op::WidenLow`/`Op::WidenHigh` of `%a0`.
pub(super) fn widen(b: &mut Body, lanes: Lanes, high: bool, half: HalfNative) -> String {
    let x = b.arg(0, "<8 x i16>");
    let w = widened(b, &x, lanes, high, half);
    b.bytes(&w, FLOATS)
}

/// `Op::NarrowTo` of `%a0` and `%a1`.
pub(super) fn narrow(b: &mut Body, to: Lanes, half: HalfNative) -> String {
    let (x, y) = (b.arg(0, FLOATS), b.arg(1, FLOATS));
    let low = narrowed(b, &x, to, half);
    let high = narrowed(b, &y, to, half);
    let all = b.inst(format!(
        "shufflevector <4 x i16> {low}, <4 x i16> {high}, <8 x i32> \
         <i32 0, i32 1, i32 2, i32 3, i32 4, i32 5, i32 6, i32 7>"
    ));
    // LLVM folds `fptrunc (fpext x)` to `x`, so after `narrow(v.widenLow(), v.widenHigh())` a
    // signalling NaN could stay signalling; `fcvtn` itself makes it quiet, and this does nothing
    // then. x86's intrinsic is not folded, and the sequences have the rule in them.
    let all = if native(to, half) && half == HalfNative::All {
        quiet_f16(b, &all)
    } else {
        all
    };
    b.bytes(&all, "<8 x i16>")
}

/// The `<8 x i16>` of f16 lanes `x` with each NaN's quiet bit set.
fn quiet_f16(b: &mut Body, x: &str) -> String {
    let splat = |v: u32| vec![format!("i16 {v}"); 8].join(", ");
    let a = b.inst(format!("and <8 x i16> {x}, <{}>", splat(0x7fff)));
    let nan = b.inst(format!("icmp ugt <8 x i16> {a}, <{}>", splat(0x7c00)));
    let mask = b.inst(format!("sext <8 x i1> {nan} to <8 x i16>"));
    let bit = b.inst(format!("and <8 x i16> {mask}, <{}>", splat(0x200)));
    b.inst(format!("or <8 x i16> {x}, {bit}"))
}

/// The scalar helper `definition` (one of `crate::half`'s sequences, whose lines are plain
/// instructions on `i1`, `i16`, `i32` and `float`) on four lanes: every type four of it, every
/// constant operand a vector of it, `.v4` after its name. Its name and definition. So the
/// sequence stays written once, and LLVM need not rediscover the vector form.
fn four_lanes(definition: &str) -> (String, String) {
    let mut name = String::new();
    let text: Vec<String> = definition
        .lines()
        .map(|l| four_lanes_line(l, &mut name))
        .collect();
    (name, text.join("\n"))
}

fn four_lanes_line(line: &str, name: &mut String) -> String {
    let wide = |ty: &str| format!("<4 x {ty}>");
    let mut out: Vec<String> = vec![];
    // The type of the operands that follow (a constant takes it).
    let mut ty = "";
    for token in line.split(' ') {
        // `@velt.f16_to_f32.soft(i16`: the helper's name and its one parameter.
        if let Some((helper, param)) = token.split_once('(') {
            *name = format!("{helper}.v4");
            out.push(format!("{name}({}", wide(param)));
            continue;
        }
        let (word, comma) = match token.strip_suffix(',') {
            Some(word) => (word, ","),
            None => (token, ""),
        };
        if matches!(word, "i1" | "i16" | "i32" | "float") {
            ty = word;
            out.push(format!("{}{comma}", wide(word)));
        } else if is_constant(word) && !ty.is_empty() {
            let lane = format!("{ty} {word}");
            out.push(format!("<{lane}, {lane}, {lane}, {lane}>{comma}"));
        } else {
            out.push(token.to_string());
        }
    }
    out.join(" ")
}

/// Whether `word` of a helper's line is a constant: a decimal integer or a hex float.
fn is_constant(word: &str) -> bool {
    word.starts_with("0x")
        || word
            .strip_prefix('-')
            .unwrap_or(word)
            .bytes()
            .all(|c| c.is_ascii_digit())
            && !word.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simd::{Arch, Caps};

    fn lines(definition: &str) -> String {
        four_lanes(definition).1
    }

    /// The sequences on four lanes: nothing scalar is left, and the constants are vectors.
    #[test]
    fn a_sequence_on_four_lanes() {
        for scalar in [F16_TO_F32_SOFT, F32_TO_F16_SOFT, BF16_TO_F32, F32_TO_BF16] {
            let (name, text) = four_lanes(scalar);
            assert!(name.ends_with(".v4"), "{name}");
            assert!(text.contains(&format!("{name}(<4 x ")), "{text}");
            for line in text.lines().skip(1).filter(|l| l.starts_with("  %")) {
                let rest = line.split_once(" = ").map_or("", |(_, r)| r);
                assert!(rest.contains("<4 x "), "scalar: {line}");
            }
        }
        let bf16 = lines(BF16_TO_F32);
        assert!(bf16.contains("zext <4 x i16> %b16 to <4 x i32>"), "{bf16}");
        assert!(
            bf16.contains("shl <4 x i32> %w, <i32 16, i32 16, i32 16, i32 16>"),
            "{bf16}"
        );
        assert!(bf16.contains("sext <4 x i1> %nan to <4 x i32>"), "{bf16}");
        assert!(bf16.contains("ret <4 x float> %f"), "{bf16}");
        let c = "<float 0x3F10000000000000, float 0x3F10000000000000, \
                 float 0x3F10000000000000, float 0x3F10000000000000>";
        let soft = lines(F16_TO_F32_SOFT);
        assert!(
            soft.contains(&format!("fsub <4 x float> %fb, {c}")),
            "{soft}"
        );
        assert!(lines(F32_TO_F16_SOFT).contains("add <4 x i32> %a, <i32 -939520001"));
    }

    fn body(symbol: &str, arch: Arch, caps: Caps) -> String {
        super::super::helper(symbol, arch, caps)
            .unwrap()
            .1
            .join("\n")
    }

    #[test]
    fn widening_and_narrowing() {
        let arm = Caps {
            half: HalfNative::All,
            ..Caps::NONE
        };
        let text = body("velt_simd_f16x8_widen_high", Arch::Neon, arm);
        assert!(text.contains("<i32 4, i32 5, i32 6, i32 7>"), "{text}");
        assert!(text.contains("fpext <4 x half>"), "{text}");
        let text = body("velt_simd_f16x8_widen_low", Arch::Sse2, Caps::NONE);
        assert!(text.contains("<i32 0, i32 1, i32 2, i32 3>"), "{text}");
        assert!(text.contains("@velt.f16_to_f32.soft.v4"), "{text}");
        assert!(
            body("velt_simd_bf16x8_widen_low", Arch::Neon, arm).contains("@velt.bf16_to_f32.v4")
        );
        let text = body("velt_simd_f32x4_narrow_f16x8", Arch::Neon, arm);
        assert_eq!(text.matches("fptrunc <4 x float>").count(), 2, "{text}");
        assert!(text.contains("sext <8 x i1>"), "{text}");
        let v3 = Caps {
            half: HalfNative::F32,
            ..Caps::NONE
        };
        let text = body("velt_simd_f32x4_narrow_f16x8", Arch::Sse2, v3);
        assert_eq!(
            text.matches("@llvm.x86.vcvtps2ph.128(<4 x float> ").count(),
            2,
            "{text}"
        );
        // The rounding is in the immediate: 4 would be whatever MXCSR says.
        assert_eq!(
            text.matches(", i32 0)").count(),
            2,
            "immediate 0, to nearest even: {text}"
        );
        assert!(
            !text.contains("fptrunc") && !text.contains("sext"),
            "{text}"
        );
        let text = body("velt_simd_f32x4_narrow_f16x8", Arch::Wasm, Caps::NONE);
        assert!(text.contains("@velt.f32_to_f16.soft.v4"), "{text}");
        assert!(
            body("velt_simd_f32x4_narrow_bf16x8", Arch::Neon, arm).contains("@velt.f32_to_bf16.v4")
        );
    }
}
