//! What the backend knows about runtime functions (rt_abi.md / rt_abi_async.md), beyond their
//! signatures:
//! - the `velt_rt_math_*` primitives whose semantics are exactly an LLVM intrinsic are emitted
//!   as that intrinsic (one instruction instead of an opaque call that clobbers memory);
//! - pure or read-only functions get `memory(none)` / `memory(read)`, so values stay in
//!   registers across them and loop-invariant code moves past them;
//! - the allocator functions carry LLVM allocator attributes (`Allocator`), like rustc's
//!   `__rust_alloc` family.

use velt_vir::vir::Ty;

/// LLVM intrinsic computing exactly what the runtime function `symbol` of type
/// `(f64) -> f64` computes. `Math.round` is not here: JS rounds ties toward +Infinity, unlike
/// `llvm.round`.
pub(crate) fn math_intrinsic(symbol: &str) -> Option<&'static str> {
    Some(match symbol {
        "velt_rt_math_sqrt" => "llvm.sqrt.f64",
        "velt_rt_math_floor" => "llvm.floor.f64",
        "velt_rt_math_ceil" => "llvm.ceil.f64",
        "velt_rt_math_trunc" => "llvm.trunc.f64",
        "velt_rt_math_fabs" => "llvm.fabs.f64",
        _ => return None,
    })
}

/// How much memory a runtime function may touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effects {
    /// Anything (the default for externs).
    Any,
    /// Reads memory, writes none (string comparison, hashing).
    ReadOnly,
    /// Reads memory and writes only state of its own that compiled code never reads: a string's
    /// position-translation table and lone-surrogate count, built on first use (#377 phase 2b:
    /// `charCodeAt` on non-ASCII text, `indexOf`).
    ReadCaching,
    /// Touches no memory (arithmetic).
    None,
}

/// Memory effects of the runtime function `symbol`.
pub(crate) fn effects(symbol: &str) -> Effects {
    match symbol {
        "velt_rt_math_sqrt" | "velt_rt_math_floor" | "velt_rt_math_ceil" | "velt_rt_math_round"
        | "velt_rt_math_trunc" | "velt_rt_math_fabs" | "velt_rt_pow_f64" | "velt_rt_pow_i64" => {
            Effects::None
        }
        "velt_rt_str_cmp"
        | "velt_rt_str_eq"
        | "velt_rt_str_hash"
        | "velt_rt_str_starts_with"
        | "velt_rt_str_ends_with" => Effects::ReadOnly,
        "velt_rt_str_char_code_at"
        | "velt_rt_str_index_of"
        | "velt_rt_str_last_index_of"
        | "velt_rt_str_includes" => Effects::ReadCaching,
        _ => Effects::Any,
    }
}

/// A function of the runtime allocator (rt_abi.md "Memory"), declared the way rustc declares
/// `__rust_alloc` / `__rust_realloc` / `__rust_dealloc`: LLVM then knows the result is a fresh
/// block of `size` bytes aligned to `align` that nothing else points to, that the calls touch
/// no program memory except the block they are given (so values loaded before a `push` that
/// grows an array stay in registers across it), and that an alloc/free pair of the same family
/// whose block is never used can be deleted. velt_rt's mem.rs upholds this: the functions only
/// call mimalloc (or the Rust global allocator) and abort on failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Allocator {
    /// `velt_rt_alloc(size: u64, align: u64) -> ptr`
    Alloc,
    /// `velt_rt_realloc(p: ptr, old_size: u64, align: u64, new_size: u64) -> ptr`
    Realloc,
    /// `velt_rt_free(p: ptr, size: u64, align: u64)`
    Free,
}

/// `"alloc-family"` shared by the three functions (a block is freed by the same family).
const ALLOC_FAMILY: &str = "\"alloc-family\"=\"velt_rt_alloc\"";

impl Allocator {
    /// The allocator role of `symbol`, if the extern has the expected signature.
    pub(crate) fn of(symbol: &str, params: &[Ty], ret: Ty) -> Option<Allocator> {
        use Ty::{Ptr, Unit, U64};
        let (kind, want_params, want_ret): (_, &[Ty], _) = match symbol {
            "velt_rt_alloc" => (Allocator::Alloc, &[U64, U64], Ptr),
            "velt_rt_realloc" => (Allocator::Realloc, &[Ptr, U64, U64, U64], Ptr),
            "velt_rt_free" => (Allocator::Free, &[Ptr, U64, U64], Unit),
            _ => return None,
        };
        (params == want_params && ret == want_ret).then_some(kind)
    }

    /// Attributes of the returned pointer (`noalias`: fresh memory, like `malloc`).
    pub(crate) fn return_attrs(self) -> &'static str {
        match self {
            Allocator::Alloc | Allocator::Realloc => "noalias noundef ",
            Allocator::Free => "",
        }
    }

    /// Attributes of param `i`: `allocptr` marks the block, `allocalign` its alignment.
    pub(crate) fn param_attrs(self, i: usize) -> &'static str {
        match (self, i) {
            (Allocator::Realloc | Allocator::Free, 0) => " noundef allocptr",
            (Allocator::Alloc, 1) | (Allocator::Realloc, 2) => " noundef allocalign",
            _ => " noundef",
        }
    }

    /// Function attributes (the contents of its attribute group). The memory effects are the
    /// ones LLVM gives libc `malloc` / `realloc` / `free`.
    pub(crate) fn function_attrs(self) -> String {
        let (kind, rest) = match self {
            Allocator::Alloc => (
                "alloc,uninitialized,aligned",
                "allocsize(0) memory(inaccessiblemem: readwrite)",
            ),
            Allocator::Realloc => (
                "realloc,aligned",
                "allocsize(3) memory(argmem: readwrite, inaccessiblemem: readwrite)",
            ),
            Allocator::Free => (
                "free",
                "memory(argmem: readwrite, inaccessiblemem: readwrite)",
            ),
        };
        format!("nounwind allockind(\"{kind}\") {rest} {ALLOC_FAMILY}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(math_intrinsic("velt_rt_math_sqrt"), Some("llvm.sqrt.f64"));
        assert_eq!(math_intrinsic("velt_rt_math_round"), None);
        assert_eq!(effects("velt_rt_math_round"), Effects::None);
        assert_eq!(effects("velt_rt_str_cmp"), Effects::ReadOnly);
        assert_eq!(effects("velt_rt_str_index_of"), Effects::ReadCaching);
        assert_eq!(effects("velt_rt_write_i64"), Effects::Any);
        assert_eq!(
            Allocator::of("velt_rt_alloc", &[Ty::U64, Ty::U64], Ty::Ptr),
            Some(Allocator::Alloc)
        );
        assert_eq!(
            Allocator::of("velt_rt_free", &[Ty::Ptr, Ty::U64, Ty::U64], Ty::Unit),
            Some(Allocator::Free)
        );
        // A different signature is not the runtime allocator.
        assert_eq!(Allocator::of("velt_rt_alloc", &[Ty::U64], Ty::Ptr), None);
    }
}
