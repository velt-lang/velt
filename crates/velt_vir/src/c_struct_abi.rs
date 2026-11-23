//! How a C call passes C structs by value (#2187 layer 3, docs/internals/design/c-ffi.md "C
//! structs"): the classification of the target's C calling convention, the one table both
//! backends follow (`vir::CAbi`). Lowering asks it once per call signature and builds what it
//! says: a struct in registers becomes one or two scalar words, a struct on the stack a pointer
//! to a copy the callee gets as its own (`byval`), a struct result in memory a pointer to the
//! caller's slot (`sret`).
//!
//! **x86-64 System V** (Linux, macOS, the BSDs), after the psABI §3.2.3 and rustc's
//! `rustc_target::callconv::x86_64`:
//! - A struct larger than 16 bytes is MEMORY. Otherwise each eightbyte is INTEGER if an integer,
//!   `bool` or pointer field overlaps it, and SSE if only floats do.
//! - An INTEGER eightbyte crosses in the next of the six integer registers, an SSE one in the next
//!   of the eight vector registers. A struct takes its registers all or none: when too few remain,
//!   the whole struct goes on the stack (MEMORY), and the registers stay for later arguments.
//! - A result larger than 16 bytes is returned through a pointer the caller passes first (which
//!   takes an integer register); otherwise INTEGER eightbytes come back in RAX then RDX, SSE ones
//!   in XMM0 then XMM1.
//!
//! An eightbyte crosses as `I64` (INTEGER) or `F64` (SSE): the register is the same whatever
//! narrower C type the eightbyte's fields have, and lowering moves the bytes through a slot
//! padded to whole eightbytes, so no byte past the struct is read or written.
//!
//! Other conventions (Win64, AAPCS64 and Apple's, wasm's) are later slices: [`supported`] says
//! which targets have one here.

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
    /// A struct in registers: these words (`I64` or `F64`), from its first eightbyte on.
    Words(Vec<Ty>),
    /// A struct on the stack: a pointer to a copy, which the callee gets as its own (`byval`).
    ByVal { size: u32, align: u32 },
}

/// How a result crosses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ret {
    /// As it is (a scalar or nothing).
    Value,
    /// A struct in registers: these words, one or two.
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

/// Does `target` have a C calling convention for structs by value here?
pub fn supported(target: &str) -> bool {
    let t = target.trim();
    t.starts_with("x86_64-") && !t.contains("-windows")
}

/// An eightbyte's class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Int,
    Sse,
}

/// The classes of `s`'s eightbytes, or `None` for MEMORY.
fn classes(s: &StructShape) -> Option<Vec<Class>> {
    if s.size > 16 || s.size == 0 {
        return None;
    }
    let words = s.size.div_ceil(8) as usize;
    let mut out = vec![None; words];
    for &(ty, off) in &s.fields {
        let size = ty.scalar_size().unwrap_or(8);
        // A field across an eightbyte boundary would be unaligned: MEMORY (C's natural layout
        // never makes one).
        if off / 8 != (off + size - 1) / 8 {
            return None;
        }
        let w = (off / 8) as usize;
        let c = if ty.is_float() {
            Class::Sse
        } else {
            Class::Int
        };
        out[w] = Some(match (out[w], c) {
            (Some(Class::Int), _) | (_, Class::Int) => Class::Int,
            _ => Class::Sse,
        });
    }
    // An eightbyte of padding only (impossible in C's natural layout): as SSE, the psABI's
    // NO_CLASS merged with its neighbour.
    Some(out.into_iter().map(|c| c.unwrap_or(Class::Sse)).collect())
}

fn word(c: Class) -> Ty {
    match c {
        Class::Int => Ty::I64,
        Class::Sse => Ty::F64,
    }
}

/// The x86-64 System V classification of a call with `args` returning `ret` (`None`: a scalar or
/// nothing).
pub fn sysv(args: &[Arg], ret: Option<&StructShape>) -> Plan {
    let (mut int, mut sse) = (6u32, 8u32);
    let ret = match ret {
        None => Ret::Value,
        Some(s) => match classes(s) {
            Some(cs) => Ret::Words(cs.into_iter().map(word).collect()),
            None => {
                int -= 1;
                Ret::Sret {
                    size: s.size,
                    align: s.align,
                }
            }
        },
    };
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        out.push(match a {
            Arg::Scalar(t) => {
                // A scalar past the registers goes on the stack on its own; it never moves a struct.
                match t.is_float() {
                    true => sse = sse.saturating_sub(1),
                    false => int = int.saturating_sub(1),
                }
                Pass::Value
            }
            Arg::Struct(s) => {
                let cs = classes(s);
                let need = |c: Class| cs.iter().flatten().filter(|&&x| x == c).count() as u32;
                match cs.as_ref() {
                    Some(cs) if need(Class::Int) <= int && need(Class::Sse) <= sse => {
                        int -= need(Class::Int);
                        sse -= need(Class::Sse);
                        Pass::Words(cs.iter().map(|&c| word(c)).collect())
                    }
                    _ => Pass::ByVal {
                        size: s.size,
                        align: s.align,
                    },
                }
            }
        });
    }
    Plan { args: out, ret }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(fields: &[(Ty, u32)], size: u32, align: u32) -> StructShape {
        StructShape {
            fields: fields.to_vec(),
            size,
            align,
        }
    }

    fn one(shape: StructShape) -> Pass {
        sysv(&[Arg::Struct(shape)], None).args.remove(0)
    }

    /// The classes clang gives these shapes on x86_64-unknown-linux-gnu.
    #[test]
    fn eightbytes_are_integer_or_sse() {
        use Ty::*;
        let words = |w: &[Ty]| Pass::Words(w.to_vec());
        assert_eq!(one(s(&[(F64, 0), (F64, 8)], 16, 8)), words(&[F64, F64]));
        assert_eq!(one(s(&[(I64, 0), (F64, 8)], 16, 8)), words(&[I64, F64]));
        assert_eq!(one(s(&[(F32, 0), (I32, 4)], 8, 4)), words(&[I64]));
        assert_eq!(one(s(&[(F32, 0), (F32, 4)], 8, 4)), words(&[F64]));
        assert_eq!(
            one(s(&[(F32, 0), (F32, 4), (F32, 8)], 12, 4)),
            words(&[F64, F64])
        );
        assert_eq!(one(s(&[(U8, 0)], 1, 1)), words(&[I64]));
        assert_eq!(one(s(&[(Bool, 0), (F32, 4)], 8, 4)), words(&[I64]));
        assert_eq!(one(s(&[(Ptr, 0), (F64, 8)], 16, 8)), words(&[I64, F64]));
        // Over 16 bytes: on the stack.
        assert_eq!(
            one(s(&[(I64, 0), (I64, 8), (I64, 16)], 24, 8)),
            Pass::ByVal { size: 24, align: 8 }
        );
    }

    /// A struct takes all its registers or none: with one integer register left, a two-INTEGER
    /// struct goes on the stack, and the register stays for the scalar after it.
    #[test]
    fn a_struct_takes_its_registers_all_or_none() {
        use Ty::*;
        let pair = Arg::Struct(s(&[(I64, 0), (I64, 8)], 16, 8));
        let mut args = vec![Arg::Scalar(I64); 5];
        args.push(pair.clone());
        args.push(Arg::Scalar(I32));
        let plan = sysv(&args, None);
        assert_eq!(plan.args[5], Pass::ByVal { size: 16, align: 8 });
        assert_eq!(plan.args[6], Pass::Value);
        // With four used, it fits.
        let mut args = vec![Arg::Scalar(I64); 4];
        args.push(pair);
        assert_eq!(sysv(&args, None).args[4], Pass::Words(vec![I64, I64]));
        // Floats count against the vector registers only.
        let floats = Arg::Struct(s(&[(F64, 0), (F64, 8)], 16, 8));
        let mut args = vec![Arg::Scalar(F64); 7];
        args.push(floats);
        assert_eq!(
            sysv(&args, None).args[7],
            Pass::ByVal { size: 16, align: 8 }
        );
    }

    /// A result of 16 bytes or less comes back in registers; a larger one in memory, whose pointer
    /// takes the first integer register.
    #[test]
    fn results_in_registers_or_memory() {
        use Ty::*;
        let small = s(&[(F64, 0), (I32, 8)], 16, 8);
        assert_eq!(sysv(&[], Some(&small)).ret, Ret::Words(vec![F64, I64]));
        let big = s(&[(I64, 0), (I64, 8), (I64, 16)], 24, 8);
        let pair = Arg::Struct(s(&[(I64, 0), (I64, 8)], 16, 8));
        let mut args = vec![Arg::Scalar(I64); 4];
        args.push(pair);
        let plan = sysv(&args, Some(&big));
        assert_eq!(plan.ret, Ret::Sret { size: 24, align: 8 });
        // The sret pointer took a register: five left, four used, one short of the pair.
        assert_eq!(plan.args[4], Pass::ByVal { size: 16, align: 8 });
    }

    #[test]
    fn only_x86_64_system_v_targets_are_supported_here() {
        assert!(supported("x86_64-unknown-linux-gnu"));
        assert!(supported("x86_64-apple-darwin"));
        assert!(!supported("x86_64-pc-windows-msvc"));
        assert!(!supported("aarch64-apple-darwin"));
        assert!(!supported("wasm32-wasip1"));
    }
}
