//! The SIMD operations: what each `velt_simd_*` extern means (rt_abi.md "SIMD").
//!
//! A vector value is a [`Ty::V128`]; its lanes are given by the operation. The SIMD API
//! (`std/simd/*.vlt`, `velt/simd`) declares one extern per vector type and operation,
//! `velt_simd_<type>_<op>`, and backends emit each call inline from the [`SimdCall`] its symbol
//! parses to. No runtime function exists behind these symbols.

mod ext;

pub use ext::{Ext, ExtOp};

use crate::vir::Ty;

/// The lanes of a vector type: `u8x16` is sixteen `U8` lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lanes {
    /// `i8x16`
    I8,
    /// `u8x16`
    U8,
    /// `i16x8`
    I16,
    /// `u16x8`
    U16,
    /// `i32x4`
    I32,
    /// `u32x4`
    U32,
    /// `i64x2`
    I64,
    /// `u64x2`
    U64,
    /// `f32x4`
    F32,
    /// `f64x2`
    F64,
    /// `f16x8`: a lane is an `f16`'s 16 bits, as `f16` is in VIR (a `U16`, never a float here).
    F16,
    /// `bf16x8`, likewise.
    BF16,
}

impl Lanes {
    /// Every vector type.
    pub const ALL: [Lanes; 12] = [
        Lanes::I8,
        Lanes::U8,
        Lanes::I16,
        Lanes::U16,
        Lanes::I32,
        Lanes::U32,
        Lanes::I64,
        Lanes::U64,
        Lanes::F32,
        Lanes::F64,
        Lanes::F16,
        Lanes::BF16,
    ];

    /// The type of one lane.
    pub fn scalar(self) -> Ty {
        match self {
            Lanes::I8 => Ty::I8,
            Lanes::U8 => Ty::U8,
            Lanes::I16 => Ty::I16,
            Lanes::U16 | Lanes::F16 | Lanes::BF16 => Ty::U16,
            Lanes::I32 => Ty::I32,
            Lanes::U32 => Ty::U32,
            Lanes::I64 => Ty::I64,
            Lanes::U64 => Ty::U64,
            Lanes::F32 => Ty::F32,
            Lanes::F64 => Ty::F64,
        }
    }

    /// Bits per lane.
    pub fn bits(self) -> u32 {
        self.scalar().scalar_size().unwrap_or(1) * 8
    }

    /// Number of lanes.
    pub fn count(self) -> u32 {
        128 / self.bits()
    }

    /// Whether the lanes are floats.
    pub fn is_float(self) -> bool {
        self.scalar().is_float()
    }

    /// Whether the lanes are signed integers.
    pub fn is_signed(self) -> bool {
        self.scalar().is_signed()
    }

    /// Whether the lanes are the storage floats (`f16x8`, `bf16x8`): bits that convert to `f32`.
    pub fn is_half(self) -> bool {
        matches!(self, Lanes::F16 | Lanes::BF16)
    }

    /// The Velt type name, `u8x16`.
    pub fn name(self) -> String {
        match self {
            Lanes::F16 => "f16x8".to_string(),
            Lanes::BF16 => "bf16x8".to_string(),
            _ => format!("{}x{}", self.scalar(), self.count()),
        }
    }

    /// The integer lanes twice as wide with the same signedness (what `WidenLow` gives).
    pub fn wider(self) -> Option<Lanes> {
        Some(match self {
            Lanes::I8 => Lanes::I16,
            Lanes::U8 => Lanes::U16,
            Lanes::I16 => Lanes::I32,
            Lanes::U16 => Lanes::U32,
            Lanes::I32 => Lanes::I64,
            Lanes::U32 => Lanes::U64,
            _ => return None,
        })
    }

    /// The integer lanes half as wide with the same signedness (what `Narrow` gives).
    pub fn narrower(self) -> Option<Lanes> {
        Lanes::ALL.into_iter().find(|l| l.wider() == Some(self))
    }

    fn named(name: &str) -> Option<Lanes> {
        Lanes::ALL.into_iter().find(|l| l.name() == name)
    }
}

/// An operation on vectors of some lane type. Integer arithmetic wraps; shifts take their count
/// modulo the lane width; comparisons give a mask (all bits of a lane set, or none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// `(lane) -> v`: every lane the same value.
    Splat,
    /// `() -> v`: each lane its own index, 0 in lane 0.
    Index,
    /// `(array: ptr, at: u64) -> bool`: whether a vector's worth of elements from `at` is
    /// inside the array (`at + lanes` does not wrap and is at most the length): what the
    /// prelude checks before `load` and `store`, and offers as a loop condition.
    Fits,
    /// `(a: ptr, b: ptr, at: u64) -> bool`: `Fits` for both arrays, as one test against the
    /// smaller length. No API names it: `velt_opt`'s `fits_pair` writes it for
    /// `fits(a, at) && fits(b, at)`, and it reads both lengths whatever `a`'s is, having tested a
    /// `b` that may be null in front of the call.
    FitsBoth,
    /// `(array: ptr, at: u64) -> v`: the lanes at elements `at..` of an array's buffer,
    /// unchecked (the prelude checks the range).
    Load,
    /// `(array: ptr, at: u64) -> v`: `Load` from `Atomic` words other threads may be storing
    /// to (velt/simd `loadOptimistic`, under a latch): a plain vector load, then frozen, so a
    /// lane a store raced is some value, never undefined behaviour; a latch's version check
    /// tells the reader whether one did. Unchecked, like `Load`.
    LoadOptimistic,
    /// `(v, array: ptr, at: u64)`: writes the lanes to elements `at..`, unchecked.
    Store,
    /// `(array: ptr, at: u64, count: u64) -> v`: elements `at..at + count` in the low lanes and
    /// 0 in the others, for `count` below the lane count, unchecked. A few scalar loads that
    /// stay inside those elements, not one per lane.
    LoadPartial,
    /// `(v, array: ptr, at: u64, count: u64)`: writes the low `count` lanes (below the lane
    /// count) to elements `at..`, unchecked, with a few scalar stores.
    StorePartial,
    /// `(v, index: u64) -> lane`: one lane; the index is in range (the prelude checks it).
    Lane,
    /// `(v, index: u64, lane) -> v`: `v` with one lane replaced; the index is in range.
    WithLane,
    /// `(a, b) -> v`: lane-wise sum.
    Add,
    /// `(a, b) -> v`: lane-wise difference.
    Sub,
    /// `(a, b) -> v`: lane-wise product. Not 8-bit lanes.
    Mul,
    /// `(a, b) -> v`: lane-wise quotient. Floats only.
    Div,
    /// `(v) -> v`: lane-wise negation. Signed integers and floats.
    Neg,
    /// `(v) -> v`: lane-wise absolute value. Signed integers below 64 bits, and floats.
    Abs,
    /// `(v) -> v`: lane-wise square root. Floats only.
    Sqrt,
    /// `(a, b) -> v`: the smaller lanes. Floats as JS `Math.min`: NaN if either lane is, and
    /// -0 below +0. Not 64-bit integers.
    Min,
    /// `(a, b) -> v`: the larger lanes, as `Min`.
    Max,
    /// `(a, b) -> v`: lane-wise sum clamped to the lane type's range. 8- and 16-bit lanes.
    AddSat,
    /// `(a, b) -> v`: lane-wise difference clamped likewise.
    SubSat,
    /// `(a, b) -> v`: bitwise AND. Integers, as `Or`, `Xor` and `Not`.
    And,
    /// `(a, b) -> v`: bitwise OR.
    Or,
    /// `(a, b) -> v`: bitwise XOR.
    Xor,
    /// `(v) -> v`: every bit flipped.
    Not,
    /// `(v, count: u32) -> v`.
    Shl,
    /// `(v, count: u32) -> v`: arithmetic for signed lanes, logical for unsigned.
    Shr,
    /// `(a, b) -> mask`: the lanes that are equal (a NaN lane equals nothing).
    Eq,
    /// `(a, b) -> mask`: the lanes that differ.
    Ne,
    /// `(a, b) -> mask`: the lanes of `a` below `b`'s; `Le`, `Gt` and `Ge` likewise. Integers
    /// by their signedness; a comparison with a NaN lane does not hold.
    Lt,
    /// `(a, b) -> mask`: at most.
    Le,
    /// `(a, b) -> mask`: above.
    Gt,
    /// `(a, b) -> mask`: at least.
    Ge,
    /// `(mask, a, b) -> v`: the lanes of `a` where the mask is set, of `b` elsewhere.
    Select,
    /// `(v) -> lane`: the sum of the lanes in the lane type, wrapping (what adding them one by
    /// one with `+` gives). Integers.
    Sum,
    /// `(v) -> wider lane`: the sum of the lanes widened (8- and 16-bit lanes to 32 bits, 32-bit
    /// ones to 64), which never overflows. 8-, 16- and 32-bit integers.
    SumWide,
    /// `(acc, a, b) -> v` on `i32x4`, `a` and `b` being `i8x16`: each lane of `acc` plus the sum
    /// of the four products of the signed bytes in the same quarter of `a` and `b` (bytes
    /// `4k..4k + 3` for lane `k`), exactly (no saturation): Arm's `sdot`.
    DotAdd,
    /// `(acc, a, b) -> v` on `f32x4`, `a` and `b` having the lanes given (`f16x8` or `bf16x8`):
    /// lane `k` is `fma(a[k + 4], b[k + 4], fma(a[k], b[k], acc[k]))`, two roundings: Arm's
    /// `fmlal` then `fmlal2` for f16 where the CPU has them, else both operands widened and two
    /// `MulAdd`s, the same bits but for which NaN a lane with several NaNs gives.
    DotAddHalf(Lanes),
    /// `(acc, a, b) -> v` on `f32x4`, `a` and `b` being `bf16x8`: `DotAddHalf`, or where `--cpu`
    /// has one, Arm's `bfdot` or x86's `vdpbf16ps`, which give lane `k`
    /// `a[2k] * b[2k] + a[2k + 1] * b[2k + 1]` and may flush subnormals.
    DotAddFast(Lanes),
    /// `(a, b, c) -> v`: `a * b + c` with one rounding (fused), the same bits on every CPU: one
    /// instruction where the CPU has FMA (arm64, x86 with `--cpu x86-64-v3`), a library call
    /// per lane elsewhere. Floats only.
    MulAdd,
    /// `(a, b, c) -> v`: `a * b + c`, fused where that is faster and rounded twice elsewhere,
    /// so the last bit may differ between CPUs. Floats only.
    MulAddFast,
    /// `(v) -> v`: the same bits as another vector type.
    As(Lanes),
    /// `(a, b) -> v`: the lanes of the low halves of `a` and `b` in turn (a0, b0, a1, b1, …).
    ZipLow,
    /// `(a, b) -> v`: the lanes of the high halves in turn.
    ZipHigh,
    /// `(v) -> wider`: the low half's lanes at twice the width (signed lanes keep their sign).
    /// 8-, 16- and 32-bit integers, and `f16`/`bf16` to `f32`, exactly, a NaN's quiet bit set.
    WidenLow,
    /// `(v) -> wider`: the high half's lanes at twice the width.
    WidenHigh,
    /// `(a, b) -> narrower`: the lanes of `a`, then of `b`, at half the width, clamped to its
    /// range. 16- and 32-bit integers.
    Narrow,
    /// `(a, b) -> v` on `f32x4`: the lanes of `a`, then of `b`, each rounded once to nearest even
    /// to the lanes given (`f16x8` or `bf16x8`), a NaN keeping its top payload bits and made
    /// quiet.
    NarrowTo(Lanes),
    /// `(v) -> v`: every lane converted to the lane type of another vector type with as many
    /// lanes: 32-bit integers to `f32` (rounded to nearest), `f32` to 32-bit integers
    /// (truncated, clamped to the range, NaN to 0).
    Convert(Lanes),
    /// `(v) -> v`: the bytes of each lane in reverse order (big-endian words to native and
    /// back). 16-, 32- and 64-bit integers.
    ReverseBytes,
    /// `(a, b) -> v`: byte `i` of the result is byte `mask[i]` of the 32 bytes of `a` followed
    /// by `b` (a constant: the symbol spells it in lanes, `velt_simd_u32x4_shuffle_1_0_7_6`,
    /// which `of_symbol` turns into bytes). Every type.
    Shuffle([u8; 16]),
}

/// An operation on a mask: one lane per vector lane, all bits set or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MaskOp {
    /// `(m) -> bool`: some lane is set.
    Any,
    /// `(m) -> bool`: every lane is set.
    All,
    /// `(m) -> u64`: bit `i` is lane `i`.
    Bitmask,
    /// `(m) -> u64`: the index of the first set lane; the lane count when none is.
    First,
    /// `(m) -> u64`: the number of set lanes.
    Count,
    /// `(a, b) -> m`: the lanes set in both.
    And,
    /// `(a, b) -> m`: the lanes set in either.
    Or,
    /// `(a, b) -> m`: the lanes set in exactly one.
    Xor,
    /// `(m) -> m`: the lanes that are not set.
    Not,
}

/// What a `velt_simd_*` symbol computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SimdCall {
    /// An operation on a vector type.
    Vector(Lanes, Op),
    /// An operation on the mask type with lanes of the given width in bits.
    Mask(u32, MaskOp),
    /// An instruction of a CPU extension (`velt_simd_u32x4_<op>`, `velt_simd_u64x2_<op>`), which
    /// the caller runs only where the CPU has the extension (std/simd/sha.vlt and clmul.vlt check
    /// `Has` first).
    Inst(ExtOp),
    /// `velt_simd_has_<ext>() -> bool`: whether the CPU has the extension (the runtime's flag;
    /// `false` on another architecture, on wasm and under Cranelift).
    Has(Ext),
    /// `velt_simd_prefetch(addr)`: start loading the cache line at `addr` for a read soon
    /// (`arrayPrefetch`); never faults. Nothing under Cranelift and on wasm.
    Prefetch,
}

impl SimdCall {
    /// The operation `symbol` names, if it is a SIMD extern for a supported combination.
    pub fn of_symbol(symbol: &str) -> Option<SimdCall> {
        let rest = symbol.strip_prefix("velt_simd_")?;
        if rest == "prefetch" {
            return Some(SimdCall::Prefetch);
        }
        if let Some(ext) = rest.strip_prefix("has_") {
            return Ext::named(&ext.replace('_', "-")).map(SimdCall::Has);
        }
        if let Some(op) = ExtOp::ALL
            .into_iter()
            .find(|o| rest == format!("{}_{}", o.type_name(), o.name()))
        {
            return Some(SimdCall::Inst(op));
        }
        let (ty, op) = rest.split_once('_')?;
        if let Some(bits) = mask_bits(ty) {
            return Some(SimdCall::Mask(bits, mask_op(op)?));
        }
        let lanes = Lanes::named(ty)?;
        let op = match op.strip_prefix("shuffle_") {
            Some(indices) => Op::Shuffle(shuffle_mask(lanes, indices)?),
            None => vector_op(op)?,
        };
        supported(lanes, op).then_some(SimdCall::Vector(lanes, op))
    }

    /// The VIR signature the extern must have.
    pub fn signature(self) -> (Vec<Ty>, Ty) {
        use Ty::{Bool, Ptr, Unit, U32, U64, V128};
        match self {
            SimdCall::Has(_) => (vec![], Bool),
            SimdCall::Prefetch => (vec![Ptr], Unit),
            SimdCall::Inst(ExtOp::Sha1H) => (vec![U32], U32),
            SimdCall::Inst(op) => {
                let mut params = vec![V128; op.vector_operands()];
                if op.takes_e() {
                    params.insert(1, U32);
                }
                (params, V128)
            }
            SimdCall::Mask(_, op) => match op {
                MaskOp::Any | MaskOp::All => (vec![V128], Bool),
                MaskOp::Bitmask | MaskOp::First | MaskOp::Count => (vec![V128], U64),
                MaskOp::And | MaskOp::Or | MaskOp::Xor => (vec![V128, V128], V128),
                MaskOp::Not => (vec![V128], V128),
            },
            SimdCall::Vector(lanes, op) => match op {
                Op::Splat => (vec![lanes.scalar()], V128),
                Op::Index => (vec![], V128),
                Op::Fits => (vec![Ptr, U64], Bool),
                Op::FitsBoth => (vec![Ptr, Ptr, U64], Bool),
                Op::Load | Op::LoadOptimistic => (vec![Ptr, U64], V128),
                Op::Store => (vec![V128, Ptr, U64], Unit),
                Op::LoadPartial => (vec![Ptr, U64, U64], V128),
                Op::StorePartial => (vec![V128, Ptr, U64, U64], Unit),
                Op::Lane => (vec![V128, U64], lanes.scalar()),
                Op::Sum => (vec![V128], lanes.scalar()),
                Op::SumWide => (vec![V128], sum_wide_lane(lanes).scalar()),
                Op::WithLane => (vec![V128, U64, lanes.scalar()], V128),
                Op::Neg
                | Op::Abs
                | Op::Sqrt
                | Op::Not
                | Op::As(_)
                | Op::WidenLow
                | Op::WidenHigh
                | Op::ReverseBytes
                | Op::Convert(_) => (vec![V128], V128),
                Op::Shl | Op::Shr => (vec![V128, U32], V128),
                Op::Select
                | Op::MulAdd
                | Op::MulAddFast
                | Op::DotAdd
                | Op::DotAddHalf(_)
                | Op::DotAddFast(_) => (vec![V128, V128, V128], V128),
                _ => (vec![V128, V128], V128),
            },
        }
    }
}

/// The lane type `Op::SumWide` sums `lanes` in: 32 bits for 8- and 16-bit lanes (as `sdot`
/// accumulates bytes), 64 for 32-bit ones, with the same signedness.
pub fn sum_wide_lane(lanes: Lanes) -> Lanes {
    match lanes {
        Lanes::I8 | Lanes::I16 => Lanes::I32,
        Lanes::U8 | Lanes::U16 => Lanes::U32,
        Lanes::I32 => Lanes::I64,
        Lanes::U32 => Lanes::U64,
        _ => panic!("ICE: sum_wide of {}", lanes.name()),
    }
}

/// The byte mask (as `Op::Shuffle` takes it, of one vector) that reverses the bytes within each
/// lane of `lanes`: what `Op::ReverseBytes` is.
pub fn reversed_bytes(lanes: Lanes) -> [u8; 16] {
    let width = (lanes.bits() / 8) as u8;
    std::array::from_fn(|i| {
        let i = i as u8;
        i - i % width + (width - 1 - i % width)
    })
}

/// The symbol of the shuffle of `lanes` that picks lane `indices[i]` (of `a`'s lanes followed by
/// `b`'s) for lane `i`.
pub fn shuffle_symbol(lanes: Lanes, indices: &[u8]) -> String {
    let spelled: Vec<String> = indices.iter().map(u8::to_string).collect();
    format!("velt_simd_{}_shuffle_{}", lanes.name(), spelled.join("_"))
}

/// The byte mask of a shuffle spelled as `lanes.count()` lane indices below twice that,
/// separated by `_` (`1_0_7_6` for a `u32x4`).
fn shuffle_mask(lanes: Lanes, spelled: &str) -> Option<[u8; 16]> {
    let count = lanes.count() as usize;
    let width = (lanes.bits() / 8) as usize;
    let indices: Vec<usize> = spelled
        .split('_')
        .map(|i| i.parse().ok())
        .collect::<Option<_>>()?;
    if indices.len() != count || indices.iter().any(|&i| i >= 2 * count) {
        return None;
    }
    let mut mask = [0u8; 16];
    for (lane, &from) in indices.iter().enumerate() {
        for byte in 0..width {
            mask[lane * width + byte] = (from * width + byte) as u8;
        }
    }
    Some(mask)
}

/// The lane width of the mask type `name` (`mask8x16`).
fn mask_bits(name: &str) -> Option<u32> {
    Some(match name {
        "mask8x16" => 8,
        "mask16x8" => 16,
        "mask32x4" => 32,
        "mask64x2" => 64,
        _ => return None,
    })
}

fn mask_op(name: &str) -> Option<MaskOp> {
    Some(match name {
        "any" => MaskOp::Any,
        "all" => MaskOp::All,
        "bitmask" => MaskOp::Bitmask,
        "first" => MaskOp::First,
        "count" => MaskOp::Count,
        "and" => MaskOp::And,
        "or" => MaskOp::Or,
        "xor" => MaskOp::Xor,
        "not" => MaskOp::Not,
        _ => return None,
    })
}

fn vector_op(name: &str) -> Option<Op> {
    if let Some(to) = name.strip_prefix("as_") {
        return Lanes::named(to).map(Op::As);
    }
    if let Some(to) = name.strip_prefix("to_") {
        return Lanes::named(to).map(Op::Convert);
    }
    if let Some(to) = name.strip_prefix("narrow_") {
        return Lanes::named(to).map(Op::NarrowTo);
    }
    if let Some(of) = name.strip_prefix("dot_add_fast_") {
        return Lanes::named(of).map(Op::DotAddFast);
    }
    if let Some(of) = name.strip_prefix("dot_add_") {
        return Lanes::named(of).map(Op::DotAddHalf);
    }
    Some(match name {
        "splat" => Op::Splat,
        "index" => Op::Index,
        "fits" => Op::Fits,
        "fits_both" => Op::FitsBoth,
        "load" => Op::Load,
        "load_optimistic" => Op::LoadOptimistic,
        "store" => Op::Store,
        "load_partial" => Op::LoadPartial,
        "store_partial" => Op::StorePartial,
        "lane" => Op::Lane,
        "with_lane" => Op::WithLane,
        "add" => Op::Add,
        "sub" => Op::Sub,
        "mul" => Op::Mul,
        "div" => Op::Div,
        "neg" => Op::Neg,
        "abs" => Op::Abs,
        "sqrt" => Op::Sqrt,
        "min" => Op::Min,
        "max" => Op::Max,
        "add_sat" => Op::AddSat,
        "sub_sat" => Op::SubSat,
        "and" => Op::And,
        "or" => Op::Or,
        "xor" => Op::Xor,
        "not" => Op::Not,
        "shl" => Op::Shl,
        "shr" => Op::Shr,
        "eq" => Op::Eq,
        "ne" => Op::Ne,
        "lt" => Op::Lt,
        "le" => Op::Le,
        "gt" => Op::Gt,
        "ge" => Op::Ge,
        "select" => Op::Select,
        "sum" => Op::Sum,
        "sum_wide" => Op::SumWide,
        "dot_add" => Op::DotAdd,
        "mul_add" => Op::MulAdd,
        "mul_add_fast" => Op::MulAddFast,
        "zip_low" => Op::ZipLow,
        "zip_high" => Op::ZipHigh,
        "widen_low" => Op::WidenLow,
        "widen_high" => Op::WidenHigh,
        "narrow" => Op::Narrow,
        "reverse_bytes" => Op::ReverseBytes,
        _ => return None,
    })
}

/// Whether `op` exists for `lanes`. The set is what the baseline of every target has as a
/// short instruction sequence (SSE2, NEON, WebAssembly SIMD): no 8-bit multiply, no 64-bit
/// integer `min`/`max`/`abs`, and unsigned types have no `neg`/`abs`.
fn supported(lanes: Lanes, op: Op) -> bool {
    let float = lanes.is_float();
    let bits = lanes.bits();
    if lanes.is_half() {
        return half_supported(op);
    }
    match op {
        Op::DotAdd => lanes == Lanes::I32,
        Op::DotAddHalf(of) | Op::NarrowTo(of) => lanes == Lanes::F32 && of.is_half(),
        Op::DotAddFast(of) => lanes == Lanes::F32 && of == Lanes::BF16,
        Op::Sum => !float,
        Op::SumWide => !float && bits <= 32,
        Op::Div | Op::Sqrt | Op::MulAdd | Op::MulAddFast => float,
        Op::Mul => float || bits != 8,
        Op::Neg => float || lanes.is_signed(),
        Op::Abs => float || (lanes.is_signed() && bits != 64),
        Op::Min | Op::Max => float || bits != 64,
        Op::AddSat | Op::SubSat => !float && bits <= 16,
        Op::And | Op::Or | Op::Xor | Op::Not | Op::Shl | Op::Shr => !float,
        Op::WidenLow | Op::WidenHigh => !float && bits <= 32,
        Op::Narrow => !float && (bits == 16 || bits == 32),
        Op::ReverseBytes => !float && bits >= 16,
        Op::Convert(to) => {
            let pair = |a: Lanes, b: Lanes| a.bits() == 32 && b == Lanes::F32 && !a.is_float();
            pair(lanes, to) || pair(to, lanes)
        }
        _ => true,
    }
}

/// Whether `f16x8` and `bf16x8` have `op`: no arithmetic and nothing that reads a lane as a number.
/// As in [`supported`], what isn't excluded exists: an operation on the bits and the array, such
/// as one `velt_opt` writes in place of `Fits`, holds for these lane types as for any other.
fn half_supported(op: Op) -> bool {
    !matches!(
        op,
        Op::Index
            | Op::LoadOptimistic
            | Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Neg
            | Op::Abs
            | Op::Sqrt
            | Op::Min
            | Op::Max
            | Op::AddSat
            | Op::SubSat
            | Op::And
            | Op::Or
            | Op::Xor
            | Op::Not
            | Op::Shl
            | Op::Shr
            | Op::Eq
            | Op::Ne
            | Op::Lt
            | Op::Le
            | Op::Gt
            | Op::Ge
            | Op::Select
            | Op::Sum
            | Op::SumWide
            | Op::DotAdd
            | Op::DotAddHalf(_)
            | Op::DotAddFast(_)
            | Op::MulAdd
            | Op::MulAddFast
            | Op::ZipLow
            | Op::ZipHigh
            | Op::Narrow
            | Op::NarrowTo(_)
            | Op::ReverseBytes
            | Op::Convert(_)
    )
}

#[cfg(test)]
mod tests;
