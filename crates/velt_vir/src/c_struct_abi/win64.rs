//! **Win64** (x86-64 Windows, MSVC's and MinGW's), after Microsoft's "x64 calling convention"
//! and clang's `WinX86_64ABIInfo`:
//! - A struct of 1, 2, 4 or 8 bytes crosses as an integer of its size, in the argument's integer
//!   register or stack slot, whatever its fields (floats too).
//! - Any other struct is passed by reference: the caller copies it and passes the copy's address
//!   in the argument's place.
//! - A result of 1, 2, 4 or 8 bytes comes back in RAX; any other is returned through a pointer the
//!   caller passes first.
//!
//! Each argument takes the register or slot of its position, so nothing here depends on what
//! came before. The integer crosses as an `I64` word of a slot padded to eight bytes, as on
//! System V: the register is the same, and the callee reads only the struct's bytes.

use super::{int_words, Arg, Pass, Plan, Ret, StructShape};

/// Does a struct of `size` bytes cross as an integer?
fn in_a_register(size: u32) -> bool {
    matches!(size, 1 | 2 | 4 | 8)
}

/// The Win64 classification of a call with `args` returning `ret`.
pub(super) fn classify(args: &[Arg], ret: Option<&StructShape>) -> Plan {
    let ret = match ret {
        None => Ret::Value,
        Some(s) if in_a_register(s.size) => Ret::Words(int_words(s.size)),
        Some(s) => Ret::Sret {
            size: s.size,
            align: s.align,
        },
    };
    let args = (args.iter())
        .map(|a| match a {
            Arg::Scalar(_) => Pass::Value,
            Arg::Struct(s) if in_a_register(s.size) => Pass::Words(int_words(s.size)),
            Arg::Struct(_) => Pass::Indirect,
        })
        .collect();
    Plan { args, ret }
}
