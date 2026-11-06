//! The CPU extensions beyond the 128-bit baseline that some SIMD operations use (#1330): the SHA
//! instructions, carry-less multiplication and the AES rounds (#2401), and the instructions of
//! each.

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
    /// The x86 AES round instructions (AES-NI) with the PCLMULQDQ, SSSE3 and SSE4.1 every such
    /// CPU has: what AES-GCM needs, so a function gated on it also has [`Ext::ClmulX86`].
    AesX86,
    /// The ARMv8 AES instructions with PMULL (FEAT_AES and FEAT_PMULL, LLVM's `aes`; FEAT_PMULL
    /// implies FEAT_AES), so a function gated on it also has [`Ext::ClmulArm`].
    AesArm,
}

impl Ext {
    /// Every extension.
    pub const ALL: [Ext; 6] = [
        Ext::ShaArm,
        Ext::ShaX86,
        Ext::ClmulX86,
        Ext::ClmulArm,
        Ext::AesX86,
        Ext::AesArm,
    ];

    /// The name `targetFeature` takes: `sha-arm`, `sha-x86`, `clmul-x86`, `clmul-arm`,
    /// `aes-x86`, `aes-arm`.
    pub fn name(self) -> &'static str {
        match self {
            Ext::ShaArm => "sha-arm",
            Ext::ShaX86 => "sha-x86",
            Ext::ClmulX86 => "clmul-x86",
            Ext::ClmulArm => "clmul-arm",
            Ext::AesX86 => "aes-x86",
            Ext::AesArm => "aes-arm",
        }
    }

    /// Whether code gated on `self` may take `other`'s check as true: `other` is `self`, or an
    /// extension every CPU with `self` has (the carry-less multiplication of an AES target).
    pub fn covers(self, other: Ext) -> bool {
        self == other
            || matches!(
                (self, other),
                (Ext::AesX86, Ext::ClmulX86) | (Ext::AesArm, Ext::ClmulArm)
            )
    }

    /// Whether the runtime keeps a flag byte for it ([`Ext::flag_symbol`]). The AES extensions
    /// have none (no new runtime code, #2241): their check is true only where the build
    /// guarantees them, and velt/simd computes the run-time answer in Velt (std/simd/aes.velt).
    pub fn has_flag(self) -> bool {
        !matches!(self, Ext::AesX86 | Ext::AesArm)
    }

    /// The extension `targetFeature(name)` names.
    pub fn named(name: &str) -> Option<Ext> {
        Ext::ALL.into_iter().find(|e| e.name() == name)
    }

    /// The runtime's byte that says whether the CPU has it (1) or not (0): set before any Velt
    /// code runs, and 0 with `VELT_SIMD_DISABLE=sha` (or `clmul`; rt_abi.md "SIMD"). Code built
    /// for a CPU that has the extension (`--cpu`) does not read it.
    pub fn flag_symbol(self) -> &'static str {
        match self {
            Ext::ShaArm => "velt_cpu_sha_arm",
            Ext::ShaX86 => "velt_cpu_sha_x86",
            Ext::ClmulX86 => "velt_cpu_clmul_x86",
            Ext::ClmulArm => "velt_cpu_clmul_arm",
            Ext::AesX86 | Ext::AesArm => "",
        }
    }

    /// Whether it is an extension of the same architecture as `other` (code gated on one runs
    /// only there, so the other architecture's checks are false in it).
    pub fn same_arch(self, other: Ext) -> bool {
        self.on_arch("aarch64") == other.on_arch("aarch64")
    }

    /// Whether its instructions exist on an architecture (`aarch64`, `x86_64`, …).
    pub fn on_arch(self, arch: &str) -> bool {
        match self {
            Ext::ShaArm | Ext::ClmulArm | Ext::AesArm => arch == "aarch64",
            Ext::ShaX86 | Ext::ClmulX86 | Ext::AesX86 => arch == "x86_64",
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
            Ext::ClmulArm | Ext::AesArm => "+aes",
            Ext::AesX86 => "+aes",
        }
    }

    /// The LLVM features of a gated function: everything a CPU with the extension has, which
    /// LLVM may use anywhere in the function.
    pub fn gated_features(self) -> &'static str {
        match self {
            Ext::ShaArm => "+sha2",
            Ext::ShaX86 => "+sha,+ssse3,+sse4.1",
            Ext::ClmulX86 => "+pclmul,+ssse3,+sse4.1",
            Ext::ClmulArm | Ext::AesArm => "+aes",
            Ext::AesX86 => "+aes,+pclmul,+ssse3,+sse4.1",
        }
    }
}

/// An instruction of a CPU extension, with the operands in the instruction's own order: the SHA
/// ones on `u32x4` values (lane 0 the low 32 bits), the carry-less multiplications on `u64x2`,
/// the AES ones on `u8x16` (byte 0 first, FIPS-197's column-major state). Every one is also
/// emulated in Velt (std/simd/sha.velt, std/simd/clmul.velt, std/simd/aes.velt), which runs where
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
    /// x86 `AESENC (state, rk)`: ShiftRows, SubBytes, MixColumns, then XOR `rk`.
    Aesenc,
    /// x86 `AESENCLAST (state, rk)`: ShiftRows, SubBytes, then XOR `rk`.
    Aesenclast,
    /// x86 `AESDEC (state, rk)`: InvShiftRows, InvSubBytes, InvMixColumns, then XOR `rk`.
    Aesdec,
    /// x86 `AESDECLAST (state, rk)`: InvShiftRows, InvSubBytes, then XOR `rk`.
    Aesdeclast,
    /// x86 `AESIMC (a)`: InvMixColumns.
    AesimcX86,
    /// x86 `AESKEYGENASSIST (a, 0)`: SubWord of words 1 and 3, each also rotated; a round
    /// constant is an XOR after it (std/simd/aes.velt), so the immediate is always 0.
    Aeskeygenassist,
    /// ARM `AESE (state, key)`: XOR `key`, then ShiftRows and SubBytes.
    Aese,
    /// ARM `AESD (state, key)`: XOR `key`, then InvShiftRows and InvSubBytes.
    Aesd,
    /// ARM `AESMC (a)`: MixColumns.
    Aesmc,
    /// ARM `AESIMC (a)`: InvMixColumns.
    AesimcArm,
}

impl ExtOp {
    /// Every operation (with each immediate of `Sha1Rnds4`).
    pub const ALL: [ExtOp; 38] = [
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
        ExtOp::Aesenc,
        ExtOp::Aesenclast,
        ExtOp::Aesdec,
        ExtOp::Aesdeclast,
        ExtOp::AesimcX86,
        ExtOp::Aeskeygenassist,
        ExtOp::Aese,
        ExtOp::Aesd,
        ExtOp::Aesmc,
        ExtOp::AesimcArm,
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
            ExtOp::Aesenc
            | ExtOp::Aesenclast
            | ExtOp::Aesdec
            | ExtOp::Aesdeclast
            | ExtOp::AesimcX86
            | ExtOp::Aeskeygenassist => Ext::AesX86,
            ExtOp::Aese | ExtOp::Aesd | ExtOp::Aesmc | ExtOp::AesimcArm => Ext::AesArm,
            _ => Ext::ShaX86,
        }
    }

    /// The operation's part of its symbol (`velt_simd_<type>_<name>`): `sha256h`, `sha1rnds4_2`,
    /// `pclmulqdq_16`, `aesenc`, `aesimc_arm`. Static text, because `SimdCall::of_symbol` compares
    /// it for every SIMD call site. An immediate outside [`ExtOp::ALL`] names nothing.
    pub fn name(self) -> &'static str {
        match self {
            ExtOp::Sha1C => "sha1c",
            ExtOp::Sha1P => "sha1p",
            ExtOp::Sha1M => "sha1m",
            ExtOp::Sha1H => "sha1h",
            ExtOp::Sha1Su0 => "sha1su0",
            ExtOp::Sha1Su1 => "sha1su1",
            ExtOp::Sha256H => "sha256h",
            ExtOp::Sha256H2 => "sha256h2",
            ExtOp::Sha256Su0 => "sha256su0",
            ExtOp::Sha256Su1 => "sha256su1",
            ExtOp::Sha256Rnds2 => "sha256rnds2",
            ExtOp::Sha256Msg1 => "sha256msg1",
            ExtOp::Sha256Msg2 => "sha256msg2",
            ExtOp::Sha1Rnds4(0) => "sha1rnds4_0",
            ExtOp::Sha1Rnds4(1) => "sha1rnds4_1",
            ExtOp::Sha1Rnds4(2) => "sha1rnds4_2",
            ExtOp::Sha1Rnds4(3) => "sha1rnds4_3",
            ExtOp::Sha1Nexte => "sha1nexte",
            ExtOp::Sha1Msg1 => "sha1msg1",
            ExtOp::Sha1Msg2 => "sha1msg2",
            ExtOp::Pclmulqdq(0) => "pclmulqdq_0",
            ExtOp::Pclmulqdq(1) => "pclmulqdq_1",
            ExtOp::Pclmulqdq(16) => "pclmulqdq_16",
            ExtOp::Pclmulqdq(17) => "pclmulqdq_17",
            ExtOp::Pmull(0) => "pmull_0",
            ExtOp::Pmull(1) => "pmull_1",
            ExtOp::Pmull(16) => "pmull_16",
            ExtOp::Pmull(17) => "pmull_17",
            ExtOp::Aesenc => "aesenc",
            ExtOp::Aesenclast => "aesenclast",
            ExtOp::Aesdec => "aesdec",
            ExtOp::Aesdeclast => "aesdeclast",
            ExtOp::AesimcX86 => "aesimc",
            ExtOp::Aeskeygenassist => "aeskeygenassist",
            ExtOp::Aese => "aese",
            ExtOp::Aesd => "aesd",
            ExtOp::Aesmc => "aesmc",
            ExtOp::AesimcArm => "aesimc_arm",
            ExtOp::Sha1Rnds4(_) | ExtOp::Pclmulqdq(_) | ExtOp::Pmull(_) => {
                panic!("ICE: {self:?} is not an instruction")
            }
        }
    }

    /// The vector type whose symbols it is among: `u32x4` (SHA), `u64x2` (carry-less) or
    /// `u8x16` (AES).
    pub fn type_name(self) -> &'static str {
        match self {
            ExtOp::Pclmulqdq(_) | ExtOp::Pmull(_) => "u64x2",
            _ if matches!(self.ext(), Ext::AesX86 | Ext::AesArm) => "u8x16",
            _ => "u32x4",
        }
    }

    /// Number of `V128` operands (`sha1c` & co. also take a `u32` second, `sha1h` only one).
    pub fn vector_operands(self) -> usize {
        match self {
            ExtOp::Sha1H => 0,
            ExtOp::AesimcX86 | ExtOp::Aeskeygenassist | ExtOp::Aesmc | ExtOp::AesimcArm => 1,
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
