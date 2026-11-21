//! The number types of HIR: integer and float widths, and the SIMD vectors of them (with the
//! masks comparing two vectors gives). Part of the HIR contract (`hir/mod.rs`).

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    ISize,
    U8,
    U16,
    U32,
    U64,
    USize,
}

impl IntTy {
    pub fn is_signed(self) -> bool {
        matches!(
            self,
            IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::ISize
        )
    }
    /// Width in bits, assuming a 64-bit target for isize/usize.
    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 | IntTy::ISize | IntTy::USize => 64,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F32,
    F64,
    /// IEEE 754 binary16, a storage type (#2049): a value is its 16-bit pattern (`u16` in
    /// VIR), it widens to `f32`/`f64`/`number` implicitly, and arithmetic on it runs in `f32`
    /// (sema widens the operands; no HIR operator has an `F16` operand).
    F16,
    /// bfloat16 (`f32`'s top 16 bits), a storage type like [`FloatTy::F16`].
    BF16,
}

impl FloatTy {
    /// Whether this is a storage type (`f16`, `bf16`), which arithmetic never runs in.
    pub fn is_storage(self) -> bool {
        matches!(self, FloatTy::F16 | FloatTy::BF16)
    }

    /// The type arithmetic on a value of this type runs in: `f32` for the storage types.
    pub fn compute(self) -> FloatTy {
        match self {
            FloatTy::F16 | FloatTy::BF16 => FloatTy::F32,
            f => f,
        }
    }
}

/// A 128-bit SIMD type (`u8x16`: sixteen `u8` lanes; `mask8x16`: what comparing two of them
/// gives, a lane that is all ones or all zeros). Copy; one `V128` in VIR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SimdTy {
    I8x16,
    U8x16,
    I16x8,
    U16x8,
    I32x4,
    U32x4,
    I64x2,
    U64x2,
    F32x4,
    F64x2,
    /// Eight `f16` lanes: bits, widening and narrowing, and lane arithmetic.
    F16x8,
    /// Eight `bf16` lanes: bits, widening and narrowing, no arithmetic.
    BF16x8,
    Mask8x16,
    Mask16x8,
    Mask32x4,
    Mask64x2,
}

/// A CPU feature a gated function is compiled for and a check tests (#1330; velt_common's table,
/// #3222 step 3): a function decorated `@targetFeature("avx2-x86", "bmi2-x86")` (any module) is
/// gated on that set ([`CpuTargets`]), and every call of it sits behind checks of those features
/// (velt/arch's `Avx2.isSupported()`, or velt/simd's older methods such as `u32x4.hasShaArm()`)
/// or in a function gated on a set that covers them, so backends may use the features anywhere in
/// it.
pub use velt_common::cpu_features::{CpuFeature as CpuTarget, CpuFeatures as CpuTargets};

/// The method of velt/simd (or std, for FMA) that checked `t` before velt/arch, by its
/// definition's full name; `None` for a feature only velt/arch checks.
pub fn cpu_check(t: CpuTarget) -> Option<&'static str> {
    Some(match t {
        CpuTarget::ShaArm => velt_common::std_item!(simd_sha, "u32x4.hasShaArm"),
        CpuTarget::ShaX86 => velt_common::std_item!(simd_sha, "u32x4.hasShaX86"),
        CpuTarget::ClmulX86 => velt_common::std_item!(simd_clmul, "u64x2.hasClmulX86"),
        CpuTarget::ClmulArm => velt_common::std_item!(simd_clmul, "u64x2.hasClmulArm"),
        CpuTarget::AesX86 => velt_common::std_item!(simd_aes, "u8x16.hasAesX86"),
        CpuTarget::AesArm => velt_common::std_item!(simd_aes, "u8x16.hasAesArm"),
        CpuTarget::FmaX86 => velt_common::std_item!(lang_math_fma_check, "hasFmaX86"),
        CpuTarget::Bf16Arm => velt_common::std_item!(simd_bf16x8, "bf16x8.hasBf16Arm"),
        _ => return None,
    })
}

/// The check of `t` as code writes it, for messages: velt/arch's `Avx2.isSupported()` (imported
/// from `velt/arch/x86` or `velt/arch/arm`).
pub fn cpu_check_call(t: CpuTarget) -> String {
    format!("{}.isSupported()", t.arch_class())
}

/// The velt/arch class definition `full` (a std function's full name) checks, if it is one:
/// `std/arch/x86/<file>::<Class>.isSupported` for a feature of x86 whose class is `<Class>`.
pub fn arch_check(full: &str) -> Option<CpuTarget> {
    let (module, item) = full.rsplit_once("::")?;
    let class = item.strip_suffix(".isSupported")?;
    let arch = if module.starts_with(velt_common::std_dir!(arch_x86)) {
        velt_common::cpu_features::CpuArch::X86_64
    } else if module.starts_with(velt_common::std_dir!(arch_arm)) {
        velt_common::cpu_features::CpuArch::Aarch64
    } else {
        return None;
    };
    (CpuTarget::ALL.iter().copied()).find(|t| t.arch() == arch && t.arch_class() == class)
}

impl SimdTy {
    /// Every SIMD type.
    pub const ALL: [SimdTy; 16] = [
        SimdTy::I8x16,
        SimdTy::U8x16,
        SimdTy::I16x8,
        SimdTy::U16x8,
        SimdTy::I32x4,
        SimdTy::U32x4,
        SimdTy::I64x2,
        SimdTy::U64x2,
        SimdTy::F32x4,
        SimdTy::F64x2,
        SimdTy::F16x8,
        SimdTy::BF16x8,
        SimdTy::Mask8x16,
        SimdTy::Mask16x8,
        SimdTy::Mask32x4,
        SimdTy::Mask64x2,
    ];

    /// The type's name in source.
    pub fn name(self) -> &'static str {
        match self {
            SimdTy::I8x16 => "i8x16",
            SimdTy::U8x16 => "u8x16",
            SimdTy::I16x8 => "i16x8",
            SimdTy::U16x8 => "u16x8",
            SimdTy::I32x4 => "i32x4",
            SimdTy::U32x4 => "u32x4",
            SimdTy::I64x2 => "i64x2",
            SimdTy::U64x2 => "u64x2",
            SimdTy::F32x4 => "f32x4",
            SimdTy::F64x2 => "f64x2",
            SimdTy::F16x8 => "f16x8",
            SimdTy::BF16x8 => "bf16x8",
            SimdTy::Mask8x16 => "mask8x16",
            SimdTy::Mask16x8 => "mask16x8",
            SimdTy::Mask32x4 => "mask32x4",
            SimdTy::Mask64x2 => "mask64x2",
        }
    }

    /// The number of lanes of a vector type (`None` for a mask type).
    pub fn lane_count(self) -> Option<u32> {
        Some(match self {
            SimdTy::I8x16 | SimdTy::U8x16 => 16,
            SimdTy::I16x8 | SimdTy::U16x8 | SimdTy::F16x8 | SimdTy::BF16x8 => 8,
            SimdTy::I32x4 | SimdTy::U32x4 | SimdTy::F32x4 => 4,
            SimdTy::I64x2 | SimdTy::U64x2 | SimdTy::F64x2 => 2,
            _ => return None,
        })
    }

    /// The SIMD type named `name`.
    pub fn named(name: &str) -> Option<SimdTy> {
        SimdTy::ALL.into_iter().find(|t| t.name() == name)
    }
}
