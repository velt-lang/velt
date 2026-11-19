//! The CPU features code can be compiled for and check at run time (#1330; the model of #3222
//! step 3, docs/internals/design/package-machine-code.md §2): one table that sema
//! (`hir::CpuTarget`, `@targetFeature`) and velt_vir (`simd::Ext`, `Function::target`) share.
//!
//! A feature is named `<name>-x86` or `<name>-arm` (the name `@targetFeature` takes) and stands
//! for one bit of the CPU's feature word ([`CpuFeature::raw_bit`]: x86's in the order of the
//! start-up check's list, rt_abi.md "CPU check", with AES appended; arm64's from bit 32) plus the
//! features it implies ([`CpuFeature::implies`]: AVX2 implies AVX implies SSE4.2, …). Its
//! [`CpuFeature::mask`] is all those bits: a check of it is true where the word holds the whole
//! mask, and code gated on a set of features may take the check of any feature whose mask the
//! set's mask holds ([`CpuFeatures::covers`]).
//!
//! The first eight are the extensions std's gated code has used since #1330; their LLVM feature
//! lists are unchanged, so that code compiles as before.

use std::fmt;

/// The architecture a feature belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CpuArch {
    X86_64,
    Aarch64,
}

impl CpuArch {
    /// Its name in a target triple (`x86_64`, `aarch64`).
    pub fn name(self) -> &'static str {
        match self {
            CpuArch::X86_64 => "x86_64",
            CpuArch::Aarch64 => "aarch64",
        }
    }
}

macro_rules! features {
    ($($(#[doc = $doc:literal])* $v:ident = $name:literal, $arch:ident, $bit:literal, [$($imp:ident),*], $op:literal, $gated:literal, $class:literal;)*) => {
        /// A CPU feature (see the module's documentation).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum CpuFeature {
            $($(#[doc = $doc])* $v,)*
        }

        impl CpuFeature {
            /// Every feature, in declaration order (its index is [`CpuFeature::index`]).
            pub const ALL: &'static [CpuFeature] = &[$(CpuFeature::$v),*];

            /// The name `@targetFeature` takes (`avx2-x86`).
            pub fn name(self) -> &'static str {
                match self { $(CpuFeature::$v => $name,)* }
            }

            /// The architecture it belongs to.
            pub fn arch(self) -> CpuArch {
                match self { $(CpuFeature::$v => CpuArch::$arch,)* }
            }

            /// Its own bit of the CPU's feature word (std/lang/cpu).
            pub fn raw_bit(self) -> u32 {
                match self { $(CpuFeature::$v => $bit,)* }
            }

            /// The features every CPU with it has, directly.
            pub fn implies(self) -> &'static [CpuFeature] {
                match self { $(CpuFeature::$v => &[$(CpuFeature::$imp),*],)* }
            }

            /// The LLVM feature a function that contains one of its instructions gets
            /// (`+pclmul`): only the feature itself, so the function stays safe on a CPU
            /// without it, each instruction being behind the check.
            pub fn op_features(self) -> &'static str {
                match self { $(CpuFeature::$v => $op,)* }
            }

            /// The LLVM features of a function gated on it: everything a CPU with it has that
            /// LLVM does not infer from the feature itself.
            pub fn gated_features(self) -> &'static str {
                match self { $(CpuFeature::$v => $gated,)* }
            }

            /// The class of velt/arch/x86 or velt/arch/arm whose `isSupported` checks it.
            pub fn arch_class(self) -> &'static str {
                match self { $(CpuFeature::$v => $class,)* }
            }
        }
    };
}

features! {
    /// The ARMv8 SHA-1 and SHA-256 instructions (FEAT_SHA256, LLVM's `sha2`).
    ShaArm = "sha-arm", Aarch64, 33, [], "+sha2", "+sha2", "Sha256";
    /// The x86 SHA extensions (SHA-NI), with the SSE4.1 every such CPU has.
    ShaX86 = "sha-x86", X86_64, 21, [Sse41X86], "+sha", "+sha,+ssse3,+sse4.1", "Sha";
    /// The x86 carry-less multiplication (PCLMULQDQ), with the SSE4.1 every such CPU has.
    ClmulX86 = "clmul-x86", X86_64, 22, [Sse41X86], "+pclmul", "+pclmul,+ssse3,+sse4.1", "Pclmulqdq";
    /// The ARMv8 64-bit polynomial multiplication (FEAT_PMULL, LLVM's `aes`).
    ClmulArm = "clmul-arm", Aarch64, 34, [], "+aes", "+aes", "Pmull";
    /// The x86 AES round instructions (AES-NI), with the PCLMULQDQ every such CPU has: what
    /// AES-GCM needs.
    AesX86 = "aes-x86", X86_64, 23, [ClmulX86], "+aes", "+aes,+pclmul,+ssse3,+sse4.1", "Aes";
    /// The ARMv8 AES instructions with PMULL (FEAT_AES, FEAT_PMULL; LLVM's `aes`).
    AesArm = "aes-arm", Aarch64, 40, [ClmulArm], "+aes", "+aes", "Aes";
    /// x86's fused multiply-add (FMA3), with the AVX it needs.
    FmaX86 = "fma-x86", X86_64, 10, [AvxX86], "+fma", "+fma,+avx,+ssse3,+sse4.1", "Fma";
    /// Arm's bf16 instructions (FEAT_BF16): `bfdot`.
    Bf16Arm = "bf16-arm", Aarch64, 39, [], "+bf16", "+bf16", "Bf16";
    /// SSE3.
    Sse3X86 = "sse3-x86", X86_64, 0, [], "+sse3", "+sse3", "Sse3";
    /// SSSE3.
    Ssse3X86 = "ssse3-x86", X86_64, 1, [Sse3X86], "+ssse3", "+ssse3", "Ssse3";
    /// SSE4.1.
    Sse41X86 = "sse4.1-x86", X86_64, 2, [Ssse3X86], "+sse4.1", "+sse4.1", "Sse41";
    /// SSE4.2 (with CRC32 and the string instructions).
    Sse42X86 = "sse4.2-x86", X86_64, 3, [Sse41X86], "+sse4.2", "+sse4.2", "Sse42";
    /// POPCNT.
    PopcntX86 = "popcnt-x86", X86_64, 4, [], "+popcnt", "+popcnt", "Popcnt";
    /// CMPXCHG16B.
    Cmpxchg16bX86 = "cmpxchg16b-x86", X86_64, 5, [], "+cx16", "+cx16", "Cmpxchg16b";
    /// AVX, with its register state saved by the operating system.
    AvxX86 = "avx-x86", X86_64, 6, [Sse42X86], "+avx", "+avx", "Avx";
    /// AVX2.
    Avx2X86 = "avx2-x86", X86_64, 7, [AvxX86], "+avx2", "+avx2", "Avx2";
    /// BMI1.
    Bmi1X86 = "bmi1-x86", X86_64, 8, [], "+bmi", "+bmi", "Bmi1";
    /// BMI2 (PDEP, PEXT, MULX, …).
    Bmi2X86 = "bmi2-x86", X86_64, 9, [], "+bmi2", "+bmi2", "Bmi2";
    /// LZCNT.
    LzcntX86 = "lzcnt-x86", X86_64, 11, [], "+lzcnt", "+lzcnt", "Lzcnt";
    /// MOVBE.
    MovbeX86 = "movbe-x86", X86_64, 12, [], "+movbe", "+movbe", "Movbe";
    /// F16C (half-precision conversions).
    F16cX86 = "f16c-x86", X86_64, 13, [AvxX86], "+f16c", "+f16c", "F16c";
    /// AVX-512 Foundation, with its register state saved by the operating system.
    Avx512fX86 = "avx512f-x86", X86_64, 14, [Avx2X86, FmaX86, F16cX86], "+avx512f", "+avx512f", "Avx512F";
    /// AVX-512 byte and word instructions.
    Avx512bwX86 = "avx512bw-x86", X86_64, 15, [Avx512fX86], "+avx512bw", "+avx512bw", "Avx512BW";
    /// AVX-512 conflict detection.
    Avx512cdX86 = "avx512cd-x86", X86_64, 16, [Avx512fX86], "+avx512cd", "+avx512cd", "Avx512CD";
    /// AVX-512 doubleword and quadword instructions.
    Avx512dqX86 = "avx512dq-x86", X86_64, 17, [Avx512fX86], "+avx512dq", "+avx512dq", "Avx512DQ";
    /// AVX-512 vector length extensions (the 128- and 256-bit forms).
    Avx512vlX86 = "avx512vl-x86", X86_64, 18, [Avx512fX86], "+avx512vl", "+avx512vl", "Avx512VL";
    /// AVX-512 vector byte manipulation.
    Avx512vbmiX86 = "avx512vbmi-x86", X86_64, 19, [Avx512bwX86], "+avx512vbmi", "+avx512vbmi", "Avx512Vbmi";
    /// AVX-512 bit algorithms.
    Avx512bitalgX86 = "avx512bitalg-x86", X86_64, 20, [Avx512bwX86], "+avx512bitalg", "+avx512bitalg", "Avx512Bitalg";
    /// The Armv8.2 dot products (FEAT_DotProd).
    DotprodArm = "dotprod-arm", Aarch64, 32, [], "+dotprod", "+dotprod", "Dp";
    /// The CRC32 instructions (FEAT_CRC32).
    CrcArm = "crc-arm", Aarch64, 35, [], "+crc", "+crc", "Crc32";
    /// The SHA-3 and SHA-512 instructions (FEAT_SHA3, FEAT_SHA512).
    Sha3Arm = "sha3-arm", Aarch64, 36, [ShaArm], "+sha3", "+sha3", "Sha3";
    /// Half-precision arithmetic (FEAT_FP16).
    Fp16Arm = "fp16-arm", Aarch64, 37, [], "+fullfp16", "+fullfp16", "Fp16";
    /// The int8 matrix multiplications (FEAT_I8MM).
    I8mmArm = "i8mm-arm", Aarch64, 38, [], "+i8mm", "+i8mm", "I8mm";
}

impl CpuFeature {
    /// Its position in [`CpuFeature::ALL`].
    pub fn index(self) -> u32 {
        self as u32
    }

    /// The feature `@targetFeature(name)` names.
    pub fn named(name: &str) -> Option<CpuFeature> {
        CpuFeature::ALL.iter().copied().find(|f| f.name() == name)
    }

    /// The part of `velt_simd_has_<…>` that names it: its name with `-` and `.` as `_`
    /// (`sse4.2-x86` is `sse4_2_x86`).
    pub fn symbol_suffix(self) -> String {
        self.name().replace(['-', '.'], "_")
    }

    /// The feature whose [`CpuFeature::symbol_suffix`] is `suffix`.
    pub fn of_symbol_suffix(suffix: &str) -> Option<CpuFeature> {
        CpuFeature::ALL
            .iter()
            .copied()
            .find(|f| f.symbol_suffix() == suffix)
    }

    /// Its own bit and every implied feature's, transitively.
    pub fn mask(self) -> u64 {
        (self.implies().iter()).fold(1u64 << self.raw_bit(), |m, f| m | f.mask())
    }

    /// Whether its instructions exist on architecture `arch` (`aarch64`, `x86_64`, …).
    pub fn on_arch(self, arch: &str) -> bool {
        self.arch().name() == arch
    }

    /// Whether code gated on `self` has `other` too: `other`'s mask is within `self`'s.
    pub fn covers(self, other: CpuFeature) -> bool {
        CpuFeatures::of(self).covers(other)
    }

    /// Whether it belongs to the same architecture as `other`.
    pub fn same_arch(self, other: CpuFeature) -> bool {
        self.arch() == other.arch()
    }

    /// The bits of the start-up check's x86 list (`velt_codegen_cl::X86_FEATURES`, raw bits 0 to
    /// 23) that code using it needs; 0 for an arm64 feature.
    pub fn x86_mask(self) -> u32 {
        match self.arch() {
            CpuArch::X86_64 => self.mask() as u32,
            CpuArch::Aarch64 => 0,
        }
    }

    /// The bits of the start-up check's arm64 list (`velt_codegen_cl::ARM_FEATURES`, raw bits 32
    /// on) that code using it needs; 0 for an x86 feature.
    pub fn arm_mask(self) -> u32 {
        match self.arch() {
            CpuArch::Aarch64 => (self.mask() >> 32) as u32,
            CpuArch::X86_64 => 0,
        }
    }

    /// The module of velt/arch whose classes check its architecture's features.
    pub fn arch_module(self) -> &'static str {
        match self.arch() {
            CpuArch::X86_64 => "velt/arch/x86",
            CpuArch::Aarch64 => "velt/arch/arm",
        }
    }
}

/// A set of CPU features: what a function is gated on (`hir::Program::cpu_targets`,
/// `vir::Function::target`). Empty for an ordinary function. Its `Debug` is `None` when empty and
/// `Some(ShaArm)` for one feature, as the `Option<Ext>` it replaced printed, so fingerprints of
/// existing code don't change.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CpuFeatures(u64);

impl CpuFeatures {
    /// No feature: an ordinary function.
    pub const NONE: CpuFeatures = CpuFeatures(0);

    /// The set of one feature.
    pub fn of(f: CpuFeature) -> CpuFeatures {
        CpuFeatures(1 << f.index())
    }

    /// `self` with `f` added.
    pub fn with(self, f: CpuFeature) -> CpuFeatures {
        CpuFeatures(self.0 | 1 << f.index())
    }

    /// Whether it holds no feature.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether it holds `f` itself (not only through an implication).
    pub fn contains(self, f: CpuFeature) -> bool {
        self.0 & 1 << f.index() != 0
    }

    /// Its features, in declaration order.
    pub fn iter(self) -> impl Iterator<Item = CpuFeature> {
        (CpuFeature::ALL.iter().copied()).filter(move |f| self.contains(*f))
    }

    /// The feature word bits it stands for: every member's [`CpuFeature::mask`].
    pub fn mask(self) -> u64 {
        self.iter().fold(0, |m, f| m | f.mask())
    }

    /// Whether code gated on it may take `f`'s check as true: `f`'s mask is within its own.
    pub fn covers(self, f: CpuFeature) -> bool {
        !self.is_empty() && f.mask() & !self.mask() == 0
    }

    /// Whether code gated on it runs only on another architecture than `f`'s, so `f`'s check is
    /// false there.
    pub fn other_arch(self, f: CpuFeature) -> bool {
        self.iter().next().is_some_and(|g| !g.same_arch(f))
    }

    /// The LLVM features of a function gated on it: its members' lists, without repeats.
    pub fn gated_features(self) -> String {
        let mut out: Vec<&str> = Vec::new();
        for f in self.iter() {
            for part in f.gated_features().split(',') {
                if !out.contains(&part) {
                    out.push(part);
                }
            }
        }
        out.join(",")
    }

    /// A name for symbols of code gated on it: the members' names with `-` and `.` as `_`,
    /// joined by `__` (`sha_arm` for one feature, as before sets).
    pub fn symbol_suffix(self) -> String {
        let parts: Vec<String> = self.iter().map(|f| f.symbol_suffix()).collect();
        parts.join("__")
    }
}

impl fmt::Debug for CpuFeatures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let all: Vec<CpuFeature> = self.iter().collect();
        match all.as_slice() {
            [] => write!(f, "None"),
            [one] => write!(f, "Some({one:?})"),
            many => write!(f, "Some({many:?})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_eight_keep_their_order_names_and_llvm_features() {
        let names: Vec<&str> = CpuFeature::ALL[..8].iter().map(|f| f.name()).collect();
        assert_eq!(
            names,
            [
                "sha-arm",
                "sha-x86",
                "clmul-x86",
                "clmul-arm",
                "aes-x86",
                "aes-arm",
                "fma-x86",
                "bf16-arm"
            ]
        );
        assert_eq!(
            CpuFeature::FmaX86.gated_features(),
            "+fma,+avx,+ssse3,+sse4.1"
        );
        assert_eq!(
            CpuFeature::AesX86.gated_features(),
            "+aes,+pclmul,+ssse3,+sse4.1"
        );
    }

    #[test]
    fn names_symbols_and_bits_are_unique() {
        for (i, a) in CpuFeature::ALL.iter().enumerate() {
            assert_eq!(a.index() as usize, i);
            assert_eq!(CpuFeature::named(a.name()), Some(*a));
            assert_eq!(CpuFeature::of_symbol_suffix(&a.symbol_suffix()), Some(*a));
            assert!(a.name().ends_with(match a.arch() {
                CpuArch::X86_64 => "-x86",
                CpuArch::Aarch64 => "-arm",
            }));
            for b in &CpuFeature::ALL[i + 1..] {
                assert_ne!(a.raw_bit(), b.raw_bit(), "{a:?} and {b:?}");
            }
            assert_eq!(a.raw_bit() < 32, a.arch() == CpuArch::X86_64, "{a:?}");
            assert!(a.implies().iter().all(|i| i.same_arch(*a)), "{a:?}");
        }
        assert!(CpuFeature::ALL.len() <= 64);
    }

    #[test]
    fn implications_cover_and_nothing_crosses_architectures() {
        use CpuFeature::*;
        assert!(Avx2X86.covers(Sse42X86) && Avx2X86.covers(AvxX86) && !AvxX86.covers(Avx2X86));
        assert!(Avx512bwX86.covers(FmaX86) && Avx512vbmiX86.covers(Avx512fX86));
        assert!(AesX86.covers(ClmulX86) && !ClmulX86.covers(AesX86));
        assert!(AesArm.covers(ClmulArm) && !ClmulArm.covers(AesArm));
        assert!(!AesArm.covers(ClmulX86) && !AesX86.covers(ClmulArm));
        assert!(!Bf16Arm.covers(ClmulArm) && !AesArm.covers(Bf16Arm));
        assert!(ShaX86.covers(Ssse3X86) && !ShaX86.covers(PopcntX86));
        let set = CpuFeatures::of(Bmi2X86).with(Avx2X86);
        assert!(set.covers(Bmi2X86) && set.covers(Sse41X86) && !set.covers(Bmi1X86));
        assert!(!CpuFeatures::NONE.covers(Sse3X86));
        assert!(set.other_arch(CrcArm) && !set.other_arch(PopcntX86));
    }

    #[test]
    fn sets_print_and_name_as_the_single_feature_did() {
        assert_eq!(format!("{:?}", CpuFeatures::NONE), "None");
        assert_eq!(
            format!("{:?}", CpuFeatures::of(CpuFeature::ShaArm)),
            "Some(ShaArm)"
        );
        assert_eq!(
            CpuFeatures::of(CpuFeature::ShaArm).symbol_suffix(),
            "sha_arm"
        );
        let two = CpuFeatures::of(CpuFeature::Bmi2X86).with(CpuFeature::Avx2X86);
        assert_eq!(two.symbol_suffix(), "avx2_x86__bmi2_x86");
        assert_eq!(two.gated_features(), "+avx2,+bmi2");
        assert_eq!(
            CpuFeatures::of(CpuFeature::AesX86)
                .with(CpuFeature::ClmulX86)
                .gated_features(),
            "+pclmul,+ssse3,+sse4.1,+aes"
        );
    }

    #[test]
    fn std_masks_match_the_table() {
        // std/lang/cpu/masks.velt spells each feature's mask as a constant; a check in std is
        // only right while they agree.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../std/lang/cpu/masks.velt");
        let text = std::fs::read_to_string(path).expect("std/lang/cpu/masks.velt");
        let mut seen = 0;
        for f in CpuFeature::ALL {
            let name = f.name().replace(['-', '.'], "_").to_uppercase();
            let line = format!("export const {name}: u64 = {:#x};", f.mask());
            assert!(text.contains(&line), "masks.velt lacks `{line}`");
            seen += 1;
        }
        assert_eq!(text.matches("export const ").count(), seen);
    }

    #[test]
    fn x86_masks_follow_the_start_up_list() {
        assert_eq!(CpuFeature::Sse42X86.x86_mask(), 0b1111);
        assert_eq!(CpuFeature::ShaX86.x86_mask(), 1 << 21 | 0b111);
        assert_eq!(CpuFeature::DotprodArm.arm_mask(), 1);
        assert_eq!(CpuFeature::DotprodArm.x86_mask(), 0);
    }
}
