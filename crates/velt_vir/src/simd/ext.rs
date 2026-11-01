//! The CPU extensions beyond the 128-bit baseline that some SIMD operations use (#1330): the SHA
//! instructions and carry-less multiplication, and the instructions of each.

/// A CPU extension beyond the 128-bit baseline that some operations use (#1330).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ext {
    /// The ARMv8 SHA-1 and SHA-256 instructions (aarch64 `sha2`).
    ShaArm,
    /// The x86 SHA extensions (SHA-NI), with the SSSE3 and SSE4.1 every such CPU has.
    ShaX86,
    /// The x86 carry-less multiplication (PCLMULQDQ), with the SSSE3 and SSE4.1 every such CPU
    /// has.
    ClmulX86,
    /// The ARMv8 64-bit polynomial multiplication (PMULL, FEAT_PMULL: LLVM's `aes`).
    ClmulArm,
}

impl Ext {
    /// Every extension.
    pub const ALL: [Ext; 4] = [Ext::ShaArm, Ext::ShaX86, Ext::ClmulX86, Ext::ClmulArm];

    /// The name `targetFeature` takes: `sha-arm`, `sha-x86`, `clmul-x86`, `clmul-arm`.
    pub fn name(self) -> &'static str {
        match self {
            Ext::ShaArm => "sha-arm",
            Ext::ShaX86 => "sha-x86",
            Ext::ClmulX86 => "clmul-x86",
            Ext::ClmulArm => "clmul-arm",
        }
    }

    /// The extension `targetFeature(name)` names.
    pub fn named(name: &str) -> Option<Ext> {
        Ext::ALL.into_iter().find(|e| e.name() == name)
    }

    /// The runtime's byte that says whether the CPU has it (1) or not (0): set before any Velt
    /// code runs, and 0 with `VELT_SIMD_DISABLE=sha` (or `clmul`; rt_abi.md "SIMD").
    pub fn flag_symbol(self) -> &'static str {
        match self {
            Ext::ShaArm => "velt_cpu_sha_arm",
            Ext::ShaX86 => "velt_cpu_sha_x86",
            Ext::ClmulX86 => "velt_cpu_clmul_x86",
            Ext::ClmulArm => "velt_cpu_clmul_arm",
        }
    }

    /// Whether its instructions exist on an architecture (`aarch64`, `x86_64`, …).
    pub fn on_arch(self, arch: &str) -> bool {
        match self {
            Ext::ShaArm | Ext::ClmulArm => arch == "aarch64",
            Ext::ShaX86 | Ext::ClmulX86 => arch == "x86_64",
        }
    }

    /// The LLVM features a function that contains one of its operations gets: only the
    /// extension itself, which LLVM uses for nothing but those operations, so such a function
    /// is safe on a CPU without it (each operation is behind the runtime check).
    pub fn op_features(self) -> &'static str {
        match self {
            Ext::ShaArm => "+sha2",
            Ext::ShaX86 => "+sha",
            Ext::ClmulX86 => "+pclmul",
            Ext::ClmulArm => "+aes",
        }
    }

    /// The LLVM features of a gated function: everything a CPU with the extension has, which
    /// LLVM may use anywhere in the function.
    pub fn gated_features(self) -> &'static str {
        match self {
            Ext::ShaArm => "+sha2",
            Ext::ShaX86 => "+sha,+ssse3,+sse4.1",
            Ext::ClmulX86 => "+pclmul,+ssse3,+sse4.1",
            Ext::ClmulArm => "+aes",
        }
    }
}

/// An instruction of a CPU extension, with the operands in the instruction's own order: the SHA
/// ones on `u32x4` values (lane 0 the low 32 bits), the carry-less multiplications on `u64x2`.
/// Every one is also emulated in Velt (std/simd/sha.vlt, std/simd/clmul.vlt), which runs where
/// the CPU lacks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExtOp {
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
    /// x86 `PCLMULQDQ (a, b, imm)`: the 128-bit carry-less product of a 64-bit lane of `a` (lane
    /// 1 if bit 0 of `imm` is set) and one of `b` (lane 1 if bit 4 is set); `imm` is 0, 1, 16 or
    /// 17, part of the operation.
    Pclmulqdq(u8),
    /// ARM `PMULL` (`PMULL2` for both high lanes) as `Pclmulqdq` takes its lanes: the 64-bit
    /// lanes of `a` and `b` that `imm` selects, multiplied without carries.
    Pmull(u8),
}

impl ExtOp {
    /// Every operation (with each immediate of `Sha1Rnds4`).
    pub const ALL: [ExtOp; 28] = [
        ExtOp::Sha1C,
        ExtOp::Sha1P,
        ExtOp::Sha1M,
        ExtOp::Sha1H,
        ExtOp::Sha1Su0,
        ExtOp::Sha1Su1,
        ExtOp::Sha256H,
        ExtOp::Sha256H2,
        ExtOp::Sha256Su0,
        ExtOp::Sha256Su1,
        ExtOp::Sha256Rnds2,
        ExtOp::Sha256Msg1,
        ExtOp::Sha256Msg2,
        ExtOp::Sha1Rnds4(0),
        ExtOp::Sha1Rnds4(1),
        ExtOp::Sha1Rnds4(2),
        ExtOp::Sha1Rnds4(3),
        ExtOp::Sha1Nexte,
        ExtOp::Sha1Msg1,
        ExtOp::Sha1Msg2,
        ExtOp::Pclmulqdq(0),
        ExtOp::Pclmulqdq(1),
        ExtOp::Pclmulqdq(16),
        ExtOp::Pclmulqdq(17),
        ExtOp::Pmull(0),
        ExtOp::Pmull(1),
        ExtOp::Pmull(16),
        ExtOp::Pmull(17),
    ];

    /// The extension it belongs to.
    pub fn ext(self) -> Ext {
        match self {
            ExtOp::Sha1C
            | ExtOp::Sha1P
            | ExtOp::Sha1M
            | ExtOp::Sha1H
            | ExtOp::Sha1Su0
            | ExtOp::Sha1Su1
            | ExtOp::Sha256H
            | ExtOp::Sha256H2
            | ExtOp::Sha256Su0
            | ExtOp::Sha256Su1 => Ext::ShaArm,
            ExtOp::Pclmulqdq(_) => Ext::ClmulX86,
            ExtOp::Pmull(_) => Ext::ClmulArm,
            _ => Ext::ShaX86,
        }
    }

    /// The operation's part of its symbol (`velt_simd_<type>_<name>`): `sha256h`, `sha1rnds4_2`,
    /// `pclmulqdq_16`.
    pub fn name(self) -> String {
        match self {
            ExtOp::Sha1Rnds4(f) => format!("sha1rnds4_{f}"),
            ExtOp::Pclmulqdq(imm) => format!("pclmulqdq_{imm}"),
            ExtOp::Pmull(imm) => format!("pmull_{imm}"),
            other => format!("{other:?}").to_lowercase(),
        }
    }

    /// The vector type whose symbols it is among: `u32x4` (SHA) or `u64x2` (carry-less).
    pub fn type_name(self) -> &'static str {
        match self {
            ExtOp::Pclmulqdq(_) | ExtOp::Pmull(_) => "u64x2",
            _ => "u32x4",
        }
    }

    /// Number of `V128` operands (`sha1c` & co. also take a `u32` second, `sha1h` only one).
    pub fn vector_operands(self) -> usize {
        match self {
            ExtOp::Sha1H => 0,
            ExtOp::Sha1Su0
            | ExtOp::Sha256H
            | ExtOp::Sha256H2
            | ExtOp::Sha256Su1
            | ExtOp::Sha256Rnds2 => 3,
            _ => 2,
        }
    }

    /// Whether a `u32` operand (ARM's `e`) comes second.
    pub(super) fn takes_e(self) -> bool {
        matches!(self, ExtOp::Sha1C | ExtOp::Sha1P | ExtOp::Sha1M)
    }
}
