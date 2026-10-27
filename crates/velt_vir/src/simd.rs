//! The SIMD operations: what each `velt_simd_*` extern means (rt_abi.md "SIMD").
//!
//! A vector value is a [`Ty::V128`]; its lanes are given by the operation. The SIMD API
//! (`std/simd/*.vlt`, `velt:simd`) declares one extern per vector type and operation,
//! `velt_simd_<type>_<op>`, and backends emit each call inline from the [`SimdCall`] its symbol
//! parses to. No runtime function exists behind these symbols.

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
}

impl Lanes {
    /// Every vector type.
    pub const ALL: [Lanes; 10] = [
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
    ];

    /// The type of one lane.
    pub fn scalar(self) -> Ty {
        match self {
            Lanes::I8 => Ty::I8,
            Lanes::U8 => Ty::U8,
            Lanes::I16 => Ty::I16,
            Lanes::U16 => Ty::U16,
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

    /// The Velt type name, `u8x16`.
    pub fn name(self) -> String {
        format!("{}x{}", self.scalar(), self.count())
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
    /// `(array: ptr, at: u64) -> v`: the lanes at elements `at..` of an array's buffer,
    /// unchecked (the prelude checks the range).
    Load,
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
    /// `(v) -> v`: the same bits as another vector type.
    As(Lanes),
    /// `(a, b) -> v`: the lanes of the low halves of `a` and `b` in turn (a0, b0, a1, b1, …).
    ZipLow,
    /// `(a, b) -> v`: the lanes of the high halves in turn.
    ZipHigh,
    /// `(v) -> wider`: the low half's lanes at twice the width (signed lanes keep their sign).
    /// 8-, 16- and 32-bit integers.
    WidenLow,
    /// `(v) -> wider`: the high half's lanes at twice the width.
    WidenHigh,
    /// `(a, b) -> narrower`: the lanes of `a`, then of `b`, at half the width, clamped to its
    /// range. 16- and 32-bit integers.
    Narrow,
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

/// A CPU extension beyond the 128-bit baseline that some operations use (#1330).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ext {
    /// The ARMv8 SHA-1 and SHA-256 instructions (aarch64 `sha2`).
    ShaArm,
    /// The x86 SHA extensions (SHA-NI), with the SSSE3 and SSE4.1 every such CPU has.
    ShaX86,
}

impl Ext {
    /// Every extension.
    pub const ALL: [Ext; 2] = [Ext::ShaArm, Ext::ShaX86];

    /// The name `__intrinsic_target` takes: `sha-arm`, `sha-x86`.
    pub fn name(self) -> &'static str {
        match self {
            Ext::ShaArm => "sha-arm",
            Ext::ShaX86 => "sha-x86",
        }
    }

    /// The extension `__intrinsic_target(name)` names.
    pub fn named(name: &str) -> Option<Ext> {
        Ext::ALL.into_iter().find(|e| e.name() == name)
    }

    /// The runtime's byte that says whether the CPU has it (1) or not (0): set before any Velt
    /// code runs, and 0 with `VELT_SIMD_DISABLE=sha` (rt_abi.md "SIMD").
    pub fn flag_symbol(self) -> &'static str {
        match self {
            Ext::ShaArm => "velt_cpu_sha_arm",
            Ext::ShaX86 => "velt_cpu_sha_x86",
        }
    }

    /// Whether its instructions exist on an architecture (`aarch64`, `x86_64`, …).
    pub fn on_arch(self, arch: &str) -> bool {
        match self {
            Ext::ShaArm => arch == "aarch64",
            Ext::ShaX86 => arch == "x86_64",
        }
    }

    /// The LLVM features a function that contains one of its operations gets: only the
    /// extension itself, which LLVM uses for nothing but those operations, so such a function
    /// is safe on a CPU without it (each operation is behind the runtime check).
    pub fn op_features(self) -> &'static str {
        match self {
            Ext::ShaArm => "+sha2",
            Ext::ShaX86 => "+sha",
        }
    }

    /// The LLVM features of a gated function: everything a CPU with the extension has, which
    /// LLVM may use anywhere in the function.
    pub fn gated_features(self) -> &'static str {
        match self {
            Ext::ShaArm => "+sha2",
            Ext::ShaX86 => "+sha,+ssse3,+sse4.1",
        }
    }
}

/// An instruction of a CPU extension, on `u32x4` values (lane 0 the low 32 bits), with the
/// operands in the instruction's own order. Every one is also emulated in Velt
/// (std/simd/sha.vlt), which runs where the CPU lacks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShaOp {
    /// ARM `SHA1C (abcd, e: u32, wk) -> abcd`: four rounds with the choose function.
    Sha1C,
    /// ARM `SHA1P`: four rounds with parity.
    Sha1P,
    /// ARM `SHA1M`: four rounds with majority.
    Sha1M,
    /// ARM `SHA1H (e: u32) -> u32`: `e` rotated left by 30.
    Sha1H,
    /// ARM `SHA1SU0 (w0, w1, w2)`: message schedule, part 1.
    Sha1Su0,
    /// ARM `SHA1SU1 (t, w3)`: message schedule, part 2.
    Sha1Su1,
    /// ARM `SHA256H (abcd, efgh, wk) -> abcd`: four rounds.
    Sha256H,
    /// ARM `SHA256H2 (efgh, abcd, wk) -> efgh`: four rounds, the other half of the state.
    Sha256H2,
    /// ARM `SHA256SU0 (w0, w1)`: message schedule, part 1.
    Sha256Su0,
    /// ARM `SHA256SU1 (t, w2, w3)`: message schedule, part 2.
    Sha256Su1,
    /// x86 `SHA256RNDS2 (cdgh, abef, wk) -> abef`: two rounds; `wk`'s two low lanes (the
    /// instruction's implicit `xmm0`).
    Sha256Rnds2,
    /// x86 `SHA256MSG1 (a, b)`.
    Sha256Msg1,
    /// x86 `SHA256MSG2 (a, b)`.
    Sha256Msg2,
    /// x86 `SHA1RNDS4 (abcd, ewk, func)`: four rounds; the immediate `func` (0–3) is part of the
    /// operation.
    Sha1Rnds4(u8),
    /// x86 `SHA1NEXTE (a, b)`.
    Sha1Nexte,
    /// x86 `SHA1MSG1 (a, b)`.
    Sha1Msg1,
    /// x86 `SHA1MSG2 (a, b)`.
    Sha1Msg2,
}

impl ShaOp {
    /// Every operation (with each immediate of `Sha1Rnds4`).
    pub const ALL: [ShaOp; 20] = [
        ShaOp::Sha1C,
        ShaOp::Sha1P,
        ShaOp::Sha1M,
        ShaOp::Sha1H,
        ShaOp::Sha1Su0,
        ShaOp::Sha1Su1,
        ShaOp::Sha256H,
        ShaOp::Sha256H2,
        ShaOp::Sha256Su0,
        ShaOp::Sha256Su1,
        ShaOp::Sha256Rnds2,
        ShaOp::Sha256Msg1,
        ShaOp::Sha256Msg2,
        ShaOp::Sha1Rnds4(0),
        ShaOp::Sha1Rnds4(1),
        ShaOp::Sha1Rnds4(2),
        ShaOp::Sha1Rnds4(3),
        ShaOp::Sha1Nexte,
        ShaOp::Sha1Msg1,
        ShaOp::Sha1Msg2,
    ];

    /// The extension it belongs to.
    pub fn ext(self) -> Ext {
        match self {
            ShaOp::Sha1C
            | ShaOp::Sha1P
            | ShaOp::Sha1M
            | ShaOp::Sha1H
            | ShaOp::Sha1Su0
            | ShaOp::Sha1Su1
            | ShaOp::Sha256H
            | ShaOp::Sha256H2
            | ShaOp::Sha256Su0
            | ShaOp::Sha256Su1 => Ext::ShaArm,
            _ => Ext::ShaX86,
        }
    }

    /// The operation's part of its symbol (`velt_simd_u32x4_<name>`): `sha256h`, `sha1rnds4_2`.
    pub fn name(self) -> String {
        match self {
            ShaOp::Sha1Rnds4(f) => format!("sha1rnds4_{f}"),
            other => format!("{other:?}").to_lowercase(),
        }
    }

    /// Number of `V128` operands (`sha1c` & co. also take a `u32` second, `sha1h` only one).
    pub fn vector_operands(self) -> usize {
        match self {
            ShaOp::Sha1H => 0,
            ShaOp::Sha1Su0
            | ShaOp::Sha256H
            | ShaOp::Sha256H2
            | ShaOp::Sha256Su1
            | ShaOp::Sha256Rnds2 => 3,
            _ => 2,
        }
    }

    /// Whether a `u32` operand (ARM's `e`) comes second.
    fn takes_e(self) -> bool {
        matches!(self, ShaOp::Sha1C | ShaOp::Sha1P | ShaOp::Sha1M)
    }
}

/// What a `velt_simd_*` symbol computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SimdCall {
    /// An operation on a vector type.
    Vector(Lanes, Op),
    /// An operation on the mask type with lanes of the given width in bits.
    Mask(u32, MaskOp),
    /// An instruction of a CPU extension (`velt_simd_u32x4_<op>`), which the caller runs only
    /// where the CPU has the extension (std/simd/sha.vlt checks `Has` first).
    Sha(ShaOp),
    /// `velt_simd_has_<ext>() -> bool`: whether the CPU has the extension (the runtime's flag;
    /// `false` on another architecture, on wasm and under Cranelift).
    Has(Ext),
    /// `velt_simd_prefetch(addr)`: start loading the cache line at `addr` for a read soon
    /// (`__intrinsic_array_prefetch`); never faults. Nothing under Cranelift and on wasm.
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
        if let Some(op) = rest.strip_prefix("u32x4_") {
            if let Some(op) = ShaOp::ALL.into_iter().find(|o| o.name() == op) {
                return Some(SimdCall::Sha(op));
            }
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
            SimdCall::Sha(ShaOp::Sha1H) => (vec![U32], U32),
            SimdCall::Sha(op) => {
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
                Op::Load => (vec![Ptr, U64], V128),
                Op::Store => (vec![V128, Ptr, U64], Unit),
                Op::LoadPartial => (vec![Ptr, U64, U64], V128),
                Op::StorePartial => (vec![V128, Ptr, U64, U64], Unit),
                Op::Lane => (vec![V128, U64], lanes.scalar()),
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
                Op::Select => (vec![V128, V128, V128], V128),
                _ => (vec![V128, V128], V128),
            },
        }
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
    Some(match name {
        "splat" => Op::Splat,
        "index" => Op::Index,
        "fits" => Op::Fits,
        "load" => Op::Load,
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
    match op {
        Op::Div | Op::Sqrt => float,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_symbols_and_signatures() {
        use Ty::{Bool, U32, V128};
        for op in ShaOp::ALL {
            let symbol = format!("velt_simd_u32x4_{}", op.name());
            assert_eq!(
                SimdCall::of_symbol(&symbol),
                Some(SimdCall::Sha(op)),
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
        assert_eq!(ShaOp::Sha256Msg2.ext(), Ext::ShaX86);
        assert_eq!(ShaOp::Sha1H.ext(), Ext::ShaArm);
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
        for unsupported in [
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
}
