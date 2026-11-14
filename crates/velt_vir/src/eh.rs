//! Catching Velt panics that unwind (docs/internals/design/unwinding.md): what the `velt_eh_*`
//! externs mean.
//!
//! A Velt panic that something catches raises Velt's own exception (class [`CLASS`]) with the
//! platform's unwinder; `catchUnwind(f)` (`hir::Intrinsic::CatchUnwind`) lowers to a call of
//! [`TRY`] with the closure's code and environment. No runtime function is behind it: a backend
//! defines `velt_eh_try` in the program's object as a function that calls `code(env)` at a call
//! site with a landing pad (LLVM `invoke`, Cranelift `try_call`) and returns null when the call
//! returned, or the exception object when a Velt exception unwound out of it. The landing pad's
//! frame names [`PERSONALITY`], which the backend defines too: it claims only exceptions of
//! [`CLASS`] (any other passes through, in the search phase already) and lands at the frame's one
//! landing pad, calling only the unwinder's own functions (`_Unwind_SetIP` & co.), no runtime.
//! On Windows x64 the unwinder is SEH ([`Catching::Seh`]): std raises the exception with
//! `RaiseException` ([`SEH_CODE`], the object's address its one parameter) and `velt_eh_try`'s
//! frame names `__C_specific_handler` with a one-entry scope table whose filter takes only that
//! code. Where nothing unwinds (WebAssembly, Windows on arm64), `velt_eh_try` is a plain call that
//! returns null.
//!
//! [`CATCHES`] says which of the two a program's `velt_eh_try` is: std declares it as an extern
//! (`declare function velt_eh_catches(): u8`) and raises Velt's exception only while a
//! `velt_eh_try` that catches runs on the thread. The JIT (`velt dev`, generator hosts) registers
//! no LSDA, so there it is 0 on targets whose objects catch.

use crate::vir::{Callee, Ty};

/// `velt_eh_try(code: ptr, env: ptr) -> ptr`: `code(env)` under a catch of Velt exceptions.
pub const TRY: &str = "velt_eh_try";

/// The personality routine of `velt_eh_try`'s frame.
pub const PERSONALITY: &str = "velt_eh_personality";

/// `velt_eh_catches() -> u8`: 1 where `velt_eh_try` catches Velt exceptions, 0 where it is a
/// plain call. Defined by the backend next to `velt_eh_try`, from the same decision.
pub const CATCHES: &str = "velt_eh_catches";

/// `velt_eh_resume()`: continues the unwind that entered the cleanup pad being run (vir.rs
/// invariant 14). Noreturn; defined by the backend (LLVM `resume`, `_Unwind_Resume` elsewhere).
pub const RESUME: &str = "velt_eh_resume";

/// Whether a call of `callee` may unwind with a Velt exception, so that a frame owning values
/// gives it a cleanup pad: a Velt function or a function value. Externs (the runtime, C) don't:
/// a panic in a Velt callback the runtime runs leaks the calling frame's values.
pub fn may_unwind(callee: &Callee) -> bool {
    matches!(callee, Callee::Func(_) | Callee::Ptr { .. })
}

/// Whether `symbol` is [`RESUME`].
pub fn is_resume(symbol: &str) -> bool {
    symbol == RESUME
}

/// `velt_eh_resume`'s VIR signature.
pub fn resume_signature() -> (Vec<Ty>, Ty) {
    (vec![], Ty::Unit)
}

/// Velt's exception class, the first word of its `_Unwind_Exception`: the bytes `VELTPANC`.
pub const CLASS: u64 = u64::from_le_bytes(*b"VELTPANC");

/// The size of the unwinder's header (`_Unwind_Exception`) on 64-bit targets: class, cleanup
/// function and two private words. Velt's payload follows it.
pub const HEADER_SIZE: u64 = 32;

/// Whether `symbol` is [`TRY`].
pub fn is_try(symbol: &str) -> bool {
    symbol == TRY
}

/// Whether `symbol` is [`CATCHES`].
pub fn is_catches(symbol: &str) -> bool {
    symbol == CATCHES
}

/// Whether the backend defines `symbol` itself (no declaration of an outside function).
pub fn is_backend_defined(symbol: &str) -> bool {
    is_try(symbol) || is_catches(symbol) || is_resume(symbol)
}

/// `velt_eh_catches`' VIR signature.
pub fn catches_signature() -> (Vec<Ty>, Ty) {
    (vec![], Ty::U8)
}

/// `velt_eh_try`'s VIR signature.
pub fn try_signature() -> (Vec<Ty>, Ty) {
    (vec![Ty::Ptr, Ty::Ptr], Ty::Ptr)
}

/// The exception code of Velt's exception under Windows' structured exceptions: `0xE0` (an
/// application error, not continuable by convention) and the bytes `VLT`.
pub const SEH_CODE: u32 = 0xE056_4C54;

/// How a target catches: with the Itanium two-phase unwinder (`_Unwind_RaiseException`, DWARF
/// CFI and GCC's LSDA: macOS, Linux and the other ELF targets), with Windows' structured
/// exceptions (x64), or not at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Catching {
    Itanium,
    /// Windows x64: `RaiseException` and a `velt_eh_try` whose frame names
    /// `__C_specific_handler` with a filter for [`SEH_CODE`]. Only `velt_eh_try` catches: calls
    /// get no cleanup pads, so the frames a panic unwinds leak what they own.
    Seh,
    /// No unwinding (WebAssembly), or not built yet (Windows on arm64).
    None,
}

impl Catching {
    /// The way a target triple catches.
    pub fn of_triple(triple: &str) -> Catching {
        if triple.starts_with("wasm") {
            Catching::None
        } else if triple.contains("windows") {
            if triple.starts_with("x86_64") {
                Catching::Seh
            } else {
                Catching::None
            }
        } else {
            Catching::Itanium
        }
    }

    /// Whether `velt_eh_try` catches on this target (in an object; the JIT registers no tables).
    pub fn catches(self) -> bool {
        self != Catching::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_spells_veltpanc() {
        assert_eq!(&CLASS.to_le_bytes(), b"VELTPANC");
    }

    #[test]
    fn targets() {
        assert_eq!(
            Catching::of_triple("aarch64-apple-darwin"),
            Catching::Itanium
        );
        assert_eq!(
            Catching::of_triple("x86_64-unknown-linux-gnu"),
            Catching::Itanium
        );
        assert_eq!(Catching::of_triple("x86_64-pc-windows-msvc"), Catching::Seh);
        assert_eq!(
            Catching::of_triple("aarch64-pc-windows-msvc"),
            Catching::None
        );
        assert_eq!(Catching::of_triple("wasm32-wasip1"), Catching::None);
    }
}
