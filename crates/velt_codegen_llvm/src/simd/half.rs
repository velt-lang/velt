//! The helpers of `f16x8` and `bf16x8`: widening to and narrowing from `f32x4`, one instruction
//! per half where the target has it (`fcvtl`, `vcvtph2ps`) and otherwise the sequences of
//! `crate::half` applied to four lanes at a time, and the dot products into `f32x4`.

use velt_vir::simd::Lanes;

use super::{Arch, Body, Caps};
use crate::half::{
    HalfNative, BF16_TO_F32, F16_TO_F32_SOFT, F32_TO_BF16, F32_TO_F16_SOFT, F64_TO_F32_ODD,
};

const FLOATS: &str = "<4 x float>";

/// Whether `lanes` convert to and from `f32` with an instruction: f16 where the target has one.
/// bf16 always takes the sequences (`crate::half`).
fn native(lanes: Lanes, half: HalfNative) -> bool {
    lanes == Lanes::F16 && half != HalfNative::None
}

/// The low (or high) four lanes of the `<8 x i16>` value `x` as `<4 x float>`, exactly.
pub(super) fn widened(b: &mut Body, x: &str, lanes: Lanes, high: bool, half: HalfNative) -> String {
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
pub(super) fn narrowed(b: &mut Body, x: &str, to: Lanes, half: HalfNative) -> String {
    if native(to, half) && half == HalfNative::All {
        let h = b.inst(format!("fptrunc {FLOATS} {x} to <4 x half>"));
        return b.inst(format!("bitcast <4 x half> {h} to <4 x i16>"));
    }
    if native(to, half) {
        // Immediate 0: to nearest even, whatever MXCSR says, as `narrow` documents (bit 2 set, as
        // in 4, would take the rounding from MXCSR instead). The soft sequences' `fadd` follows
        // MXCSR, so the paths agree at its default, which no Velt API changes.
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

/// `Op::DotAddHalf`: `fmlal` then `fmlal2` for f16 where the CPU has them (the same two fused
/// operations in the same order), otherwise both operands widened and two `llvm.fma`.
pub(super) fn dot_add(b: &mut Body, of: Lanes, caps: Caps) -> String {
    if of == Lanes::F16 && caps.fhm {
        return fmlal(b);
    }
    chain(b, of, caps.half)
}

/// `Op::DotAddFast`: the CPU's bf16 dot product where `--cpu` has one, else `Op::DotAddHalf`.
pub(super) fn dot_add_fast(b: &mut Body, of: Lanes, arch: Arch, caps: Caps) -> String {
    let intrinsic = match arch {
        Arch::Neon if caps.bf16_dot => "llvm.aarch64.neon.bfdot.v4f32.v8bf16",
        Arch::Sse2 if caps.bf16_dot => "llvm.x86.avx512bf16.dpbf16ps.128",
        _ => return dot_add(b, of, caps),
    };
    let acc = b.arg(0, FLOATS);
    let x = b.arg(1, "<8 x bfloat>");
    let y = b.arg(2, "<8 x bfloat>");
    b.declarations.push(format!(
        "declare {FLOATS} @{intrinsic}({FLOATS}, <8 x bfloat>, <8 x bfloat>)"
    ));
    let r = b.inst(format!(
        "call {FLOATS} @{intrinsic}({FLOATS} {acc}, <8 x bfloat> {x}, <8 x bfloat> {y})"
    ));
    b.bytes(&r, FLOATS)
}

fn fmlal(b: &mut Body) -> String {
    let acc = b.arg(0, FLOATS);
    let x = b.arg(1, "<8 x half>");
    let y = b.arg(2, "<8 x half>");
    let mut r = acc;
    for name in ["fmlal", "fmlal2"] {
        let intrinsic = format!("llvm.aarch64.neon.{name}.v4f32.v8f16");
        b.declarations.push(format!(
            "declare {FLOATS} @{intrinsic}({FLOATS}, <8 x half>, <8 x half>)"
        ));
        r = b.inst(format!(
            "call {FLOATS} @{intrinsic}({FLOATS} {r}, <8 x half> {x}, <8 x half> {y})"
        ));
    }
    b.bytes(&r, FLOATS)
}

fn chain(b: &mut Body, of: Lanes, half: HalfNative) -> String {
    let acc = b.arg(0, FLOATS);
    let x = b.arg(1, "<8 x i16>");
    let y = b.arg(2, "<8 x i16>");
    let mut r = acc;
    for high in [false, true] {
        let p = widened(b, &x, of, high, half);
        let q = widened(b, &y, of, high, half);
        r = b.intrinsic("fma", "v4f32", FLOATS, &[&p, &q, &r], "");
    }
    b.bytes(&r, FLOATS)
}

/// The four `double`s `x` as `float`s rounded to odd, which round to `f16` as `x` itself would
/// (`crate::half`).
pub(super) fn narrowed_to_odd(b: &mut Body, x: &str) -> String {
    let (name, definition) = four_lanes(F64_TO_F32_ODD);
    b.declarations.push(definition);
    b.inst(format!("call {FLOATS} {name}(<4 x double> {x})"))
}

/// The scalar helper `definition` (one of `crate::half`'s sequences, whose lines are plain
/// instructions on `i1`, `i16`, `i32`, `i64`, `float` and `double`) on four lanes: every type four of it, every
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
        if matches!(word, "i1" | "i16" | "i32" | "i64" | "float" | "double") {
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

    /// Which instructions each target gets.
    #[test]
    fn the_target_picks_the_instructions() {
        let f16 = "velt_simd_f32x4_dot_add_f16x8";
        let bf16 = "velt_simd_f32x4_dot_add_bf16x8";
        let fast = "velt_simd_f32x4_dot_add_fast_bf16x8";
        let arm = Caps {
            half: HalfNative::All,
            ..Caps::NONE
        };
        let m1 = Caps { fhm: true, ..arm };
        let m2 = Caps {
            bf16_dot: true,
            ..m1
        };
        let v3 = Caps {
            half: HalfNative::F32,
            ..Caps::NONE
        };
        let bf16_cpu = Caps {
            bf16_dot: true,
            ..v3
        };

        let text = body(f16, Arch::Neon, m1);
        assert!(
            text.contains("@llvm.aarch64.neon.fmlal.v4f32.v8f16"),
            "{text}"
        );
        assert!(
            text.contains("@llvm.aarch64.neon.fmlal2.v4f32.v8f16"),
            "{text}"
        );
        assert!(!text.contains("llvm.fma"), "{text}");
        // The low lanes first, as `dotAdd` documents: the order shows in the rounding.
        let low = text.find("call <4 x float> @llvm.aarch64.neon.fmlal.v4f32.v8f16");
        let high = text.find("call <4 x float> @llvm.aarch64.neon.fmlal2.v4f32.v8f16");
        assert!(matches!((low, high), (Some(l), Some(h)) if l < h), "{text}");
        // No FEAT_FHM: the conversion instruction and two FMAs.
        let text = body(f16, Arch::Neon, arm);
        assert!(
            !text.contains("fmlal") && text.contains("fpext <4 x half>"),
            "{text}"
        );
        assert_eq!(
            text.matches("call <4 x float> @llvm.fma.v4f32").count(),
            2,
            "{text}"
        );
        let text = body(f16, Arch::Sse2, v3);
        assert!(
            text.contains("fpext <4 x half>") && text.contains("@llvm.fma"),
            "{text}"
        );
        let text = body(f16, Arch::Sse2, Caps::NONE);
        assert!(
            text.contains("@velt.f16_to_f32.soft.v4") && text.contains("@llvm.fma"),
            "{text}"
        );
        let text = body(f16, Arch::Wasm, Caps::NONE);
        assert!(text.contains("@velt.f16_to_f32.soft.v4"), "{text}");
        // bf16's exact form has no instruction anywhere, whatever the CPU.
        for caps in [m2, bf16_cpu, Caps::NONE] {
            let text = body(bf16, Arch::Neon, caps);
            assert!(
                text.contains("@velt.bf16_to_f32.v4") && text.contains("@llvm.fma"),
                "{text}"
            );
            assert!(!text.contains("bfdot") && !text.contains("fmlal"), "{text}");
        }
        // The fast form uses the CPU's dot product only where it has one.
        assert!(body(fast, Arch::Neon, m2).contains("@llvm.aarch64.neon.bfdot.v4f32.v8bf16"));
        assert!(body(fast, Arch::Sse2, bf16_cpu).contains("@llvm.x86.avx512bf16.dpbf16ps.128"));
        for (arch, caps) in [(Arch::Neon, m1), (Arch::Sse2, v3), (Arch::Wasm, Caps::NONE)] {
            let text = body(fast, arch, caps);
            assert!(
                text.contains("@llvm.fma") && !text.contains("bfdot"),
                "{text}"
            );
            assert!(!text.contains("dpbf16ps"), "{text}");
        }
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
