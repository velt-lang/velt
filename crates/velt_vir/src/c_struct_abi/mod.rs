//! How a C call passes C structs by value (#2187 layer 3, docs/internals/design/c-ffi.md "C
//! structs"): the classification of the target's C calling convention, the one table both
//! backends follow (`vir::CAbi`). Lowering asks it once per call signature and builds what it
//! says: a struct in registers becomes scalar words, a struct on the stack a pointer to a copy the
//! callee gets as its own (`byval`) or a pointer to a copy the caller keeps (passed by
//! reference), a struct result in memory a pointer to the caller's slot (`sret`).
//!
//! One module per convention: x86-64 System V (sysv.rs: Linux, macOS, the BSDs), Win64
//! (win64.rs: x86-64 Windows) and AAPCS64 (aapcs64.rs: arm64 Linux, and Apple's variant). wasm's
//! and Windows on arm64's are later slices: [`supported`] says which targets have one here.

mod aapcs64;
mod sysv;
#[cfg(test)]
mod tests;
mod win64;

use crate::vir::Ty;

/// What one argument of a C call is, for the classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arg {
    /// A scalar: an integer, `bool`, a float or a pointer.
    Scalar(Ty),
    /// A C struct: its scalar fields, nested structs flattened, at their offsets, and its size and
    /// alignment.
    Struct(StructShape),
}

/// A C struct, flattened: every scalar field with its offset, the size and the alignment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructShape {
    pub fields: Vec<(Ty, u32)>,
    pub size: u32,
    pub align: u32,
}

/// How an argument crosses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pass {
    /// As it is (a scalar).
    Value,
    /// A struct in registers: these words, the struct's bytes from its start, laid out as their
    /// aggregate lays them out (`I64` or `F64` eightbytes, or an AAPCS64 HFA's `F32`s).
    Words(Vec<Ty>),
    /// A struct on the stack as `words`, after `pad`: zeros filling the registers left of its
    /// class (AAPCS64, where a struct that doesn't fit takes no register and leaves none for
    /// the arguments after it).
    Stack { pad: Vec<Ty>, words: Vec<Ty> },
    /// A struct on the stack: a pointer to a copy, which the callee gets as its own (`byval`,
    /// System V).
    ByVal { size: u32, align: u32 },
    /// A struct by reference: a pointer to a copy the caller makes, passed as a pointer argument
    /// (Win64, and AAPCS64's structs over 16 bytes).
    Indirect,
}

/// How a result crosses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ret {
    /// As it is (a scalar or nothing).
    Value,
    /// A struct in registers: these words, one to four, laid out as [`Pass::Words`]'s.
    Words(Vec<Ty>),
    /// A struct in memory: the caller passes a pointer to its slot first (`sret`).
    Sret { size: u32, align: u32 },
}

/// The classification of one call signature: each argument's and the result's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub args: Vec<Pass>,
    pub ret: Ret,
}

/// A C calling convention with a classification here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conv {
    SysV,
    Win64,
    /// AAPCS64; `apple` for Apple's variant, which packs stack arguments by their size.
    Aapcs64 {
        apple: bool,
    },
}

/// The convention C structs by value follow on `target`, if one is here.
pub fn conv(target: &str) -> Option<Conv> {
    let t = target.trim();
    let windows = t.contains("-windows");
    if t.starts_with("x86_64-") {
        return Some(if windows { Conv::Win64 } else { Conv::SysV });
    }
    if t.starts_with("aarch64-") || t.starts_with("arm64-") {
        if t.contains("-apple-") {
            return Some(Conv::Aapcs64 { apple: true });
        }
        if t.contains("-linux") {
            return Some(Conv::Aapcs64 { apple: false });
        }
    }
    None
}

/// Does `target` have a C calling convention for structs by value here?
pub fn supported(target: &str) -> bool {
    conv(target).is_some()
}

/// The classification of a call with `args` returning `ret` (`None`: a scalar or nothing) in
/// convention `conv`.
pub fn classify(conv: Conv, args: &[Arg], ret: Option<&StructShape>) -> Plan {
    match conv {
        Conv::SysV => sysv::classify(args, ret),
        Conv::Win64 => win64::classify(args, ret),
        Conv::Aapcs64 { apple } => aapcs64::classify(args, ret, apple),
    }
}

/// Eightbyte words of `I64` covering `size` bytes.
fn int_words(size: u32) -> Vec<Ty> {
    vec![Ty::I64; size.div_ceil(8) as usize]
}
