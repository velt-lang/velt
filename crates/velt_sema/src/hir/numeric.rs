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
    Mask8x16,
    Mask16x8,
    Mask32x4,
    Mask64x2,
}

/// A CPU extension a gated function is compiled for (#1330): its first statement is
/// `__intrinsic_target("sha-arm")` (std only), and every call of it sits behind the check of
/// that extension (`u32x4.hasShaArm()`), so backends may use the extension anywhere in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CpuTarget {
    /// The ARMv8 SHA instructions.
    ShaArm,
    /// The x86 SHA extensions (with SSSE3 and SSE4.1).
    ShaX86,
}

impl CpuTarget {
    /// Every target.
    pub const ALL: [CpuTarget; 2] = [CpuTarget::ShaArm, CpuTarget::ShaX86];

    /// The name `__intrinsic_target` takes.
    pub fn name(self) -> &'static str {
        match self {
            CpuTarget::ShaArm => "sha-arm",
            CpuTarget::ShaX86 => "sha-x86",
        }
    }

    /// The target named `name`.
    pub fn named(name: &str) -> Option<CpuTarget> {
        CpuTarget::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The static method of `u32x4` that checks for it (`velt:simd`).
    pub fn check(self) -> &'static str {
        match self {
            CpuTarget::ShaArm => "hasShaArm",
            CpuTarget::ShaX86 => "hasShaX86",
        }
    }
}

impl SimdTy {
    /// Every SIMD type.
    pub const ALL: [SimdTy; 14] = [
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
            SimdTy::I16x8 | SimdTy::U16x8 => 8,
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
