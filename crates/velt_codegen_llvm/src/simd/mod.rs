//! The `velt_simd_*` externs (`velt_vir::simd`) as `alwaysinline` helpers defined in the
//! module: each reinterprets its `<16 x i8>` arguments as the operation's lanes, applies one
//! LLVM vector instruction or intrinsic, and reinterprets the result back. After inlining
//! nothing of the helper or the reinterpretation is left.

use velt_vir::simd::{reversed_bytes, Lanes, Op, SimdCall};
use velt_vir::vir::Ty;

use crate::types::{abi_ret, abi_type};
use masks::mask;
use reshape::{convert, narrow, widen, zip};

mod masks;
mod partial;
mod reshape;
mod sha;

pub(crate) use sha::has_ext;

// Alias tags (tbaa.rs): the header word a helper reads, and the elements it loads or stores.
const HEADER: crate::tbaa::Access = crate::tbaa::Access::Header;
const ELEMENT: crate::tbaa::Access = crate::tbaa::Access::Element;

/// The instruction sets a helper's body depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arch {
    /// arm64: NEON.
    Neon,
    /// x86-64: SSE2.
    Sse2,
    /// WebAssembly: its SIMD instructions (`simd128`), asked for function by function
    /// ([`WASM_SIMD`]), so only a program that uses vectors needs an engine that has them.
    Wasm,
}

impl Arch {
    /// The architecture of `target`.
    pub(crate) fn of(target: &crate::target::Target) -> Arch {
        if target.is_wasm() {
            Arch::Wasm
        } else if target.has_neon() {
            Arch::Neon
        } else {
            // x86-64, the one other architecture `target::normalize` accepts.
            Arch::Sse2
        }
    }
}

/// The attribute of the functions of a WebAssembly program that uses vectors: without it LLVM
/// expands every vector operation into scalar code, several times slower. The helpers have
/// it, and so has every function of such a program (`Emitter::uses_simd`).
pub(crate) const WASM_SIMD: &str = " \"target-features\"=\"+simd128\"";

/// The helper computing the SIMD extern `symbol` for `arch`: its name, what the module must
/// define and declare for it, and the signature the extern must have.
pub(crate) fn helper(symbol: &str, arch: Arch) -> Option<(String, Vec<String>, Vec<Ty>, Ty)> {
    let call = SimdCall::of_symbol(symbol)?;
    let (params, ret) = call.signature();
    let name = format!("@velt.simd.{}", symbol.strip_prefix("velt_simd_")?);
    let mut body = Body::default();
    let result = match call {
        SimdCall::Vector(lanes, Op::Min | Op::Max) if lanes.is_float() && arch == Arch::Sse2 => {
            Some(float_min_max(&mut body, lanes, call))
        }
        SimdCall::Vector(lanes, op) => vector(&mut body, lanes, op),
        SimdCall::Mask(bits, op) => mask(&mut body, bits, op, arch == Arch::Neon),
        SimdCall::Has(ext) => Some(sha::has(&mut body, arch, ext)),
        SimdCall::Sha(op) => Some(sha::op(&mut body, arch, op)),
        SimdCall::Prefetch => {
            // A read, kept in every cache level (`locality` 3), of data (cache type 1).
            body.declarations.push(
                "declare void @llvm.prefetch.p0(ptr, i32 immarg, i32 immarg, i32 immarg)".into(),
            );
            body.lines
                .push_str("  call void @llvm.prefetch.p0(ptr %a0, i32 0, i32 3, i32 1)\n");
            None
        }
    };
    let args: Vec<String> = params
        .iter()
        .enumerate()
        .map(|(i, p)| Some(format!("{} %a{i}", abi_type(*p).ok()?)))
        .collect::<Option<_>>()?;
    let ret_line = match result {
        Some(value) => format!("ret {} {value}", crate::types::llvm_type(ret)?),
        None => "ret void".to_string(),
    };
    let ext_features;
    let features = match call {
        _ if arch == Arch::Wasm => WASM_SIMD,
        SimdCall::Sha(op) if has_ext(arch, op.ext()) => {
            ext_features = format!(" \"target-features\"=\"{}\"", op.ext().op_features());
            ext_features.as_str()
        }
        _ => "",
    };
    let definition = format!(
        "define internal {} {name}({}) alwaysinline nounwind{features} {{\n{}  {ret_line}\n}}",
        abi_ret(ret).ok()?,
        args.join(", "),
        body.lines
    );
    body.declarations.push(definition);
    Some((name, body.declarations, params, ret))
}

/// The instructions of a helper and the intrinsics they call.
#[derive(Default)]
struct Body {
    lines: String,
    declarations: Vec<String>,
    values: u32,
}

impl Body {
    /// Emit `%vN = <instruction>` and give its name.
    fn inst(&mut self, instruction: String) -> String {
        let name = format!("%v{}", self.values);
        self.values += 1;
        self.lines.push_str(&format!("  {name} = {instruction}\n"));
        name
    }

    /// Argument `i` as lanes of type `ty`.
    fn arg(&mut self, i: usize, ty: &str) -> String {
        self.inst(format!("bitcast <16 x i8> %a{i} to {ty}"))
    }

    /// A value of lanes `ty` as the `<16 x i8>` every helper returns.
    fn bytes(&mut self, value: &str, ty: &str) -> String {
        self.inst(format!("bitcast {ty} {value} to <16 x i8>"))
    }

    /// Call the intrinsic `llvm.<name>.<suffix>` on values of type `ty`.
    fn intrinsic(
        &mut self,
        name: &str,
        suffix: &str,
        ty: &str,
        args: &[&str],
        extra: &str,
    ) -> String {
        let params: Vec<&str> = args.iter().map(|_| ty).collect();
        let extra_param = extra.split(' ').next().filter(|s| !s.is_empty());
        let mut declared: Vec<&str> = params.clone();
        declared.extend(extra_param);
        self.declarations.push(format!(
            "declare {ty} @llvm.{name}.{suffix}({})",
            declared.join(", ")
        ));
        let mut passed: Vec<String> = args.iter().map(|a| format!("{ty} {a}")).collect();
        if !extra.is_empty() {
            passed.push(extra.to_string());
        }
        self.inst(format!(
            "call {ty} @llvm.{name}.{suffix}({})",
            passed.join(", ")
        ))
    }
}

/// LLVM spelling of one lane and of the vector (`i8`, `<16 x i8>`), and the vector's
/// intrinsic-name suffix (`v16i8`).
fn spell(lanes: Lanes) -> (&'static str, String, String) {
    let (lane, short) = match lanes {
        Lanes::F32 => ("float", "f32".to_string()),
        Lanes::F64 => ("double", "f64".to_string()),
        _ => (int_type(lanes.bits()), format!("i{}", lanes.bits())),
    };
    let n = lanes.count();
    (lane, format!("<{n} x {lane}>"), format!("v{n}{short}"))
}

fn int_type(bits: u32) -> &'static str {
    match bits {
        8 => "i8",
        16 => "i16",
        32 => "i32",
        _ => "i64",
    }
}

/// The integer vector with lanes as wide as `lanes`' (what a mask is).
fn mask_type(bits: u32) -> String {
    format!("<{} x {}>", 128 / bits, int_type(bits))
}

/// The body of a vector operation; the value to return, if any.
fn vector(b: &mut Body, lanes: Lanes, op: Op) -> Option<String> {
    let (lane, ty, _) = spell(lanes);
    let value = match op {
        Op::Store => {
            let x = b.arg(0, &ty);
            let p = element(b, lane, 1);
            let align = lanes.bits() / 8;
            let tag = ELEMENT.suffix();
            b.lines
                .push_str(&format!("  store {ty} {x}, ptr {p}, align {align}{tag}\n"));
            return None;
        }
        Op::StorePartial => {
            partial::store(b, lanes);
            return None;
        }
        Op::LoadPartial => return Some(partial::load(b, lanes)),
        // These give something other than this vector type.
        Op::Fits => return Some(fits(b, lanes.count())),
        Op::As(_) => return Some("%a0".to_string()),
        Op::Lane => {
            let x = b.arg(0, &ty);
            return Some(b.inst(format!("extractelement {ty} {x}, i64 %a1")));
        }
        Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge => return Some(compare(b, lanes, op)),
        Op::WidenLow | Op::WidenHigh => return Some(widen(b, lanes, op == Op::WidenHigh)),
        Op::Narrow => return Some(narrow(b, lanes)),
        Op::Convert(to) => return Some(convert(b, lanes, to)),
        // Byte shuffles: the helper's own `<16 x i8>` arguments, nothing to reinterpret.
        Op::ReverseBytes => return Some(byte_shuffle(b, &reversed_bytes(lanes), false)),
        Op::Shuffle(mask) => return Some(byte_shuffle(b, &mask, true)),
        Op::Splat => splat(b, lanes, "%a0"),
        Op::Index => index(b, lanes),
        Op::Load => {
            let p = element(b, lane, 0);
            let tag = ELEMENT.suffix();
            b.inst(format!(
                "load {ty}, ptr {p}, align {}{tag}",
                lanes.bits() / 8
            ))
        }
        Op::WithLane => {
            let x = b.arg(0, &ty);
            b.inst(format!("insertelement {ty} {x}, {lane} %a2, i64 %a1"))
        }
        Op::ZipLow | Op::ZipHigh => zip(b, lanes, op == Op::ZipHigh),
        Op::Neg | Op::Abs | Op::Sqrt | Op::Not => unary(b, lanes, op),
        Op::Shl | Op::Shr => shift(b, lanes, op),
        Op::Select => select(b, lanes),
        _ => {
            let (x, y) = (b.arg(0, &ty), b.arg(1, &ty));
            binary(b, lanes, op, &x, &y)
        }
    };
    Some(b.bytes(&value, &ty))
}

/// The bytes `mask` picks from the arguments (`%a0` followed by `%a1`, or only `%a0`).
fn byte_shuffle(b: &mut Body, mask: &[u8; 16], two: bool) -> String {
    let picks: Vec<String> = mask.iter().map(|m| format!("i32 {m}")).collect();
    let second = if two { "%a1" } else { "poison" };
    b.inst(format!(
        "shufflevector <16 x i8> %a0, <16 x i8> {second}, <16 x i32> <{}>",
        picks.join(", ")
    ))
}

/// The vector whose lanes are their own indices.
fn index(b: &mut Body, lanes: Lanes) -> String {
    let (lane, ty, _) = spell(lanes);
    let values: Vec<String> = (0..lanes.count())
        .map(|k| match lanes.is_float() {
            true => format!(
                "{lane} {}",
                crate::types::float_literal(lanes.scalar(), f64::from(k))
            ),
            false => format!("{lane} {k}"),
        })
        .collect();
    // An instruction (not a constant operand), so that every operation's value is a name.
    b.inst(format!(
        "select i1 true, {ty} <{}>, {ty} zeroinitializer",
        values.join(", ")
    ))
}

/// A vector of `lanes` with every lane the scalar `value`.
fn splat(b: &mut Body, lanes: Lanes, value: &str) -> String {
    let (lane, ty, _) = spell(lanes);
    let n = lanes.count();
    let one = b.inst(format!("insertelement {ty} poison, {lane} {value}, i64 0"));
    b.inst(format!(
        "shufflevector {ty} {one}, {ty} poison, <{n} x i32> zeroinitializer"
    ))
}

/// `neg`, `abs`, `sqrt` or `not` of `%a0`.
fn unary(b: &mut Body, lanes: Lanes, op: Op) -> String {
    let (_, ty, suffix) = spell(lanes);
    let float = lanes.is_float();
    let x = b.arg(0, &ty);
    match op {
        Op::Neg if float => b.inst(format!("fneg {ty} {x}")),
        Op::Neg => b.inst(format!("sub {ty} zeroinitializer, {x}")),
        Op::Abs if float => b.intrinsic("fabs", &suffix, &ty, &[&x], ""),
        Op::Abs => b.intrinsic("abs", &suffix, &ty, &[&x], "i1 false"),
        Op::Sqrt => b.intrinsic("sqrt", &suffix, &ty, &[&x], ""),
        _ => not(b, &ty, lanes.count(), &x),
    }
}

/// `%a0` shifted by `%a1` bits, taken modulo the lane width (LLVM's shifts are undefined past
/// it): arithmetic to the right for signed lanes.
fn shift(b: &mut Body, lanes: Lanes, op: Op) -> String {
    let (lane, ty, _) = spell(lanes);
    let x = b.arg(0, &ty);
    let bits = lanes.bits();
    let masked = b.inst(format!("and i32 %a1, {}", bits - 1));
    let count = match bits {
        64 => b.inst(format!("zext i32 {masked} to i64")),
        32 => masked,
        _ => b.inst(format!("trunc i32 {masked} to {lane}")),
    };
    let counts = splat(b, lanes, &count);
    let instruction = match op {
        Op::Shl => "shl",
        _ if lanes.is_signed() => "ashr",
        _ => "lshr",
    };
    b.inst(format!("{instruction} {ty} {x}, {counts}"))
}

/// The mask of the lanes where `%a0 <op> %a1` holds, as the bytes a helper returns.
fn compare(b: &mut Body, lanes: Lanes, op: Op) -> String {
    let (_, ty, _) = spell(lanes);
    let (x, y) = (b.arg(0, &ty), b.arg(1, &ty));
    let test = comparison(op, lanes.is_float(), lanes.is_signed());
    let c = b.inst(format!("{test} {ty} {x}, {y}"));
    let n = lanes.count();
    let m = mask_type(lanes.bits());
    let wide = b.inst(format!("sext <{n} x i1> {c} to {m}"));
    b.bytes(&wide, &m)
}

/// The lanes of `%a1` where the mask `%a0` is set, of `%a2` elsewhere.
fn select(b: &mut Body, lanes: Lanes) -> String {
    let (_, ty, _) = spell(lanes);
    let m = mask_type(lanes.bits());
    let mask = b.arg(0, &m);
    let (x, y) = (b.arg(1, &ty), b.arg(2, &ty));
    let c = b.inst(format!("icmp slt {m} {mask}, zeroinitializer"));
    let n = lanes.count();
    b.inst(format!("select <{n} x i1> {c}, {ty} {x}, {ty} {y}"))
}

/// Every bit of `x` flipped. The all-ones operand is built from a comparison, since the
/// `splat` constant syntax needs a recent LLVM.
fn not(b: &mut Body, ty: &str, n: u32, x: &str) -> String {
    let same = b.inst(format!("icmp eq {ty} {x}, {x}"));
    let ones = b.inst(format!("sext <{n} x i1> {same} to {ty}"));
    b.inst(format!("xor {ty} {x}, {ones}"))
}

/// Whether the `n` elements from `%a1` are inside the array `%a0` (its length is the second
/// word of its header): the length is at least `n` and `%a1` at most the length minus `n`.
/// Nothing in it can wrap, unlike `%a1 + n <= len`, so LLVM 18 too removes the range checks of
/// the loads a loop on `fits(xs, i + 48)` makes at `i`, `i + 16` and `i + 32`.
fn fits(b: &mut Body, n: u32) -> String {
    let at = b.inst("getelementptr inbounds i8, ptr %a0, i64 8".to_string());
    let tag = HEADER.suffix();
    let len = b.inst(format!("load i64, ptr {at}, align 8{tag}"));
    let room = b.inst(format!("icmp uge i64 {len}, {n}"));
    let last = b.inst(format!("sub i64 {len}, {n}"));
    let inside = b.inst(format!("icmp ule i64 %a1, {last}"));
    let both = b.inst(format!("and i1 {room}, {inside}"));
    b.inst(format!("zext i1 {both} to i8"))
}

/// Address of element `%a{array + 1}` of the array whose header `%a{array}` points to (the
/// `{ data, len, cap }` header of docs/internals/design/semantics-stage2.md).
fn element(b: &mut Body, lane: &str, array: usize) -> String {
    let tag = HEADER.suffix();
    let data = b.inst(format!("load ptr, ptr %a{array}, align 8{tag}"));
    b.inst(format!(
        "getelementptr inbounds {lane}, ptr {data}, i64 %a{}",
        array + 1
    ))
}

fn comparison(op: Op, float: bool, signed: bool) -> String {
    let (ordered, int) = match op {
        Op::Eq => ("oeq", "eq"),
        Op::Ne => ("une", "ne"),
        Op::Lt => ("olt", "lt"),
        Op::Le => ("ole", "le"),
        Op::Gt => ("ogt", "gt"),
        _ => ("oge", "ge"),
    };
    if float {
        format!("fcmp {ordered}")
    } else if matches!(op, Op::Eq | Op::Ne) {
        format!("icmp {int}")
    } else {
        format!("icmp {}{int}", if signed { 's' } else { 'u' })
    }
}

/// The lane-wise arithmetic or bitwise operation `op` on `x` and `y`.
fn binary(b: &mut Body, lanes: Lanes, op: Op, x: &str, y: &str) -> String {
    let (_, ty, suffix) = spell(lanes);
    let (ty, suffix) = (ty.as_str(), suffix.as_str());
    let (float, signed) = (lanes.is_float(), lanes.is_signed());
    let sign = if signed { 's' } else { 'u' };
    let instruction = match op {
        Op::Add if float => "fadd",
        Op::Sub if float => "fsub",
        Op::Mul if float => "fmul",
        Op::Div => "fdiv",
        Op::Add => "add",
        Op::Sub => "sub",
        Op::Mul => "mul",
        Op::And => "and",
        Op::Or => "or",
        Op::Xor => "xor",
        Op::Min if float => return b.intrinsic("minimum", suffix, ty, &[x, y], ""),
        Op::Max if float => return b.intrinsic("maximum", suffix, ty, &[x, y], ""),
        Op::Min => return b.intrinsic(&format!("{sign}min"), suffix, ty, &[x, y], ""),
        Op::Max => return b.intrinsic(&format!("{sign}max"), suffix, ty, &[x, y], ""),
        Op::AddSat => return b.intrinsic(&format!("{sign}add.sat"), suffix, ty, &[x, y], ""),
        _ => return b.intrinsic(&format!("{sign}sub.sat"), suffix, ty, &[x, y], ""),
    };
    b.inst(format!("{instruction} {ty} {x}, {y}"))
}

/// Float `min` or `max` (`call`) as JS defines them, from SSE2's operations: the smaller (or
/// larger) lane by an ordered comparison; where the lanes are equal, their bits combined, so
/// that `-0` is below `0`; where either is NaN, their sum, a NaN. `llvm.minimum` computes the
/// same, but x86-64 has no instruction for it and older LLVM cannot expand it for vectors.
fn float_min_max(b: &mut Body, lanes: Lanes, call: SimdCall) -> String {
    let (_, ty, _) = spell(lanes);
    let n = lanes.count();
    let ints = mask_type(lanes.bits());
    let min = call == SimdCall::Vector(lanes, Op::Min);
    let (x, y) = (b.arg(0, &ty), b.arg(1, &ty));
    let (xi, yi) = (b.arg(0, &ints), b.arg(1, &ints));
    let first = b.inst(format!(
        "fcmp {} {ty} {x}, {y}",
        if min { "olt" } else { "ogt" }
    ));
    let picked = b.inst(format!("select <{n} x i1> {first}, {ty} {x}, {ty} {y}"));
    let bits = b.inst(format!(
        "{} {ints} {xi}, {yi}",
        if min { "or" } else { "and" }
    ));
    let zeros = b.inst(format!("bitcast {ints} {bits} to {ty}"));
    let equal = b.inst(format!("fcmp oeq {ty} {x}, {y}"));
    let ordered = b.inst(format!(
        "select <{n} x i1> {equal}, {ty} {zeros}, {ty} {picked}"
    ));
    let nan = b.inst(format!("fcmp uno {ty} {x}, {y}"));
    let sum = b.inst(format!("fadd {ty} {x}, {y}"));
    let result = b.inst(format!(
        "select <{n} x i1> {nan}, {ty} {sum}, {ty} {ordered}"
    ));
    b.bytes(&result, &ty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_helper_per_supported_symbol() {
        let (name, definitions, params, ret) = helper("velt_simd_u8x16_eq", Arch::Wasm).unwrap();
        assert_eq!(name, "@velt.simd.u8x16_eq");
        assert_eq!((params, ret), (vec![Ty::V128, Ty::V128], Ty::V128));
        let text = definitions.last().unwrap();
        assert!(text.contains("icmp eq <16 x i8>"), "{text}");
        assert!(text.contains("alwaysinline"), "{text}");
        assert!(helper("velt_simd_u8x16_mul", Arch::Wasm).is_none());
        assert!(helper("velt_rt_math_sqrt", Arch::Wasm).is_none());
    }

    #[test]
    fn float_min_on_sse2_is_built_from_comparisons() {
        let (_, definitions, _, _) = helper("velt_simd_f64x2_min", Arch::Sse2).unwrap();
        assert_eq!(definitions.len(), 1, "no intrinsic is declared");
        let text = &definitions[0];
        assert!(text.contains("fcmp olt <2 x double>"), "{text}");
        assert!(text.contains("or <2 x i64>"), "{text}");
        let (_, definitions, _, _) = helper("velt_simd_u8x16_min", Arch::Sse2).unwrap();
        assert!(definitions[0].contains("@llvm.umin.v16i8"));
    }

    #[test]
    fn lanes_are_rearranged_with_shuffles() {
        let text = |symbol: &str| helper(symbol, Arch::Neon).unwrap().1.pop().unwrap();
        let zip = text("velt_simd_u32x4_zip_high");
        assert!(
            zip.contains("<4 x i32> <i32 2, i32 6, i32 3, i32 7>"),
            "{zip}"
        );
        let widen = text("velt_simd_i8x16_widen_low");
        assert!(
            widen.contains("sext <8 x i8>") && widen.contains("to <8 x i16>"),
            "{widen}"
        );
        let narrow = text("velt_simd_u16x8_narrow");
        assert!(
            narrow.contains("@llvm.umin.v16i16") && narrow.contains("to <16 x i8>"),
            "{narrow}"
        );
        let convert = text("velt_simd_f32x4_to_u32x4");
        assert!(
            convert.contains("@llvm.fptoui.sat.v4i32.v4f32"),
            "{convert}"
        );
    }

    #[test]
    fn intrinsics_are_declared() {
        let (_, definitions, _, _) = helper("velt_simd_f32x4_min", Arch::Wasm).unwrap();
        assert_eq!(
            definitions[0],
            "declare <4 x float> @llvm.minimum.v4f32(<4 x float>, <4 x float>)"
        );
        let (_, definitions, _, _) = helper("velt_simd_i16x8_abs", Arch::Wasm).unwrap();
        assert_eq!(
            definitions[0],
            "declare <8 x i16> @llvm.abs.v8i16(<8 x i16>, i1)"
        );
    }
}
