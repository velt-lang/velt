//! **AAPCS64** (arm64 Linux) and **Apple's arm64** variant, after the "Procedure Call Standard
//! for the Arm 64-bit Architecture" §6.8 and clang's `AArch64ABIInfo`:
//! - A homogeneous floating-point aggregate (HFA: one to four fields, all `f32` or all `f64`)
//!   crosses in that many consecutive vector registers, each holding one field.
//! - Any other struct of 16 bytes or less crosses in one or two consecutive integer registers,
//!   its bytes as eightbytes.
//! - A struct takes its registers all or none: when too few remain, it goes on the stack, and the
//!   registers of its class left are used up too (NSRN or NGRN set to 8), so the arguments
//!   after it of that class go on the stack as well. Lowering spends them on zero arguments
//!   ([`Pass::Stack`]'s `pad`), and the struct's words go on the stack after them.
//! - A larger struct is passed by reference, its copy's address taking an integer register.
//! - A result comes back as it would be passed (an HFA in v0-v3, 16 bytes or less in x0 and x1);
//!   a larger one is returned through a pointer the caller passes in x8, which takes no argument
//!   register.
//!
//! On the stack, AAPCS64 gives a struct a slot aligned to 8 and its fields their offsets in it.
//! Its words do the same as separate stack arguments: `F64`s and `I64`s take eight bytes each,
//! and an `f32` HFA crosses as its bytes in `F64` eightbytes (vector words, which go on the
//! stack with the vector registers used up, where integer words would take a general register). Apple's variant packs stack
//! arguments by their size and alignment, as it lays an `f32` HFA out (four bytes each, aligned
//! to 4), so there the `F32` fields cross as they are.

use super::{int_words, Arg, Pass, Plan, Ret, StructShape};
use crate::vir::Ty;

/// Registers of each class.
const REGS: u32 = 8;

/// `s`'s field type and count if it is an HFA.
fn hfa(s: &StructShape) -> Option<(Ty, u32)> {
    let &(base, _) = s.fields.first()?;
    let n = s.fields.len() as u32;
    let size = base.scalar_size()?;
    let packed =
        (s.fields.iter().enumerate()).all(|(k, &(t, off))| t == base && off == k as u32 * size);
    (matches!(base, Ty::F32 | Ty::F64) && (1..=4).contains(&n) && packed && s.size == n * size)
        .then_some((base, n))
}

/// The AAPCS64 classification (Apple's variant if `apple`) of a call with `args` returning
/// `ret`.
pub(super) fn classify(args: &[Arg], ret: Option<&StructShape>, apple: bool) -> Plan {
    let ret = match ret {
        None => Ret::Value,
        Some(s) => match hfa(s) {
            Some((base, n)) => Ret::Words(vec![base; n as usize]),
            None if s.size <= 16 => Ret::Words(int_words(s.size)),
            None => Ret::Sret {
                size: s.size,
                align: s.align,
            },
        },
    };
    // Registers used so far: general (NGRN) and vector (NSRN).
    let (mut ngrn, mut nsrn) = (0u32, 0u32);
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        out.push(match a {
            Arg::Scalar(t) => {
                // A scalar past the registers goes on the stack on its own.
                match t.is_float() {
                    true => nsrn = (nsrn + 1).min(REGS),
                    false => ngrn = (ngrn + 1).min(REGS),
                }
                Pass::Value
            }
            Arg::Struct(s) => match hfa(s) {
                Some((base, n)) if nsrn + n <= REGS => {
                    nsrn += n;
                    Pass::Words(vec![base; n as usize])
                }
                Some((base, n)) => {
                    let pad = vec![Ty::F64; (REGS - nsrn) as usize];
                    nsrn = REGS;
                    let words = match (base, apple) {
                        // Vector words, so they go on the stack too: an integer word would
                        // take a general register still free.
                        (Ty::F32, false) => vec![Ty::F64; s.size.div_ceil(8) as usize],
                        _ => vec![base; n as usize],
                    };
                    Pass::Stack { pad, words }
                }
                None if s.size > 16 => {
                    ngrn = (ngrn + 1).min(REGS);
                    Pass::Indirect
                }
                None => {
                    let words = int_words(s.size);
                    let n = words.len() as u32;
                    if ngrn + n <= REGS {
                        ngrn += n;
                        Pass::Words(words)
                    } else {
                        let pad = vec![Ty::I64; (REGS - ngrn) as usize];
                        ngrn = REGS;
                        Pass::Stack { pad, words }
                    }
                }
            },
        });
    }
    Plan { args: out, ret }
}
