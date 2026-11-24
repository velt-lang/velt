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

use super::{Arg, Pass, Plan, Ret, StructShape};
use crate::vir::Ty;

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

/// The x86-64 System V classification of a call with `args` returning `ret`.
pub(super) fn classify(args: &[Arg], ret: Option<&StructShape>) -> Plan {
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
