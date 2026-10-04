//! What the backend knows about runtime functions (rt_abi.md / rt_abi_async.md), beyond their
//! signatures:
//! - the `velt_rt_math_*` primitives whose semantics are exactly an LLVM intrinsic are emitted
//!   as that intrinsic (one instruction instead of an opaque call that clobbers memory);
//! - JS's ToInt32 and `Math.clz32` are emitted through small `alwaysinline` helpers defined in
//!   the module (`inline_helper`): a guarded conversion with the runtime call only on a cold
//!   path, and one `ctlz`;
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

/// JS ToInt32 (`velt_rt_math_to_int32`): for |x| < 2^63 one `fptosi` to `i64` and a truncation,
/// which is exact (ToInt32 is the value modulo 2^32); NaN, ±Infinity and larger values take the
/// cold call. The guard compares the double itself, so a value converted from a 32-bit integer
/// folds to that integer.
const TO_INT32: &str = "define internal i32 @velt.to_int32(double %x) alwaysinline nounwind {
  %lo = fcmp oge double %x, 0xC3E0000000000000
  %hi = fcmp olt double %x, 0x43E0000000000000
  %in = and i1 %lo, %hi
  br i1 %in, label %fast, label %slow, !prof !{!\"branch_weights\", i32 2000, i32 1}
fast:
  %t = fptosi double %x to i64
  %r = trunc i64 %t to i32
  ret i32 %r
slow:
  %s = call i32 @velt_rt_math_to_int32(double %x)
  ret i32 %s
}";

/// JS `(a * b) | 0` on int32 numbers (`velt_rt_math_mul_int32`): the exact `i64` product; within
/// 2^53 its low 32 bits, past it converted to a double and back first, which rounds it exactly
/// like the double multiply (|p| < 2^62, so the conversions are in range).
const MUL_INT32: &str = "define internal i32 @velt.mul_int32(i32 %a, i32 %b) alwaysinline nounwind {
  %x = sext i32 %a to i64
  %y = sext i32 %b to i64
  %p = mul nsw i64 %x, %y
  %q = add i64 %p, 9007199254740992
  %small = icmp ule i64 %q, 18014398509481984
  br i1 %small, label %exact, label %round
exact:
  %r = trunc i64 %p to i32
  ret i32 %r
round:
  %d = sitofp i64 %p to double
  %t = fptosi double %d to i64
  %r2 = trunc i64 %t to i32
  ret i32 %r2
}";

/// JS `(a + x) | 0` for an int32 `a` and a double `x` (`velt_rt_math_add_int32`): when `x` is a
/// whole number of at most 2^52 the double sum is exact, so it is the 32-bit sum of `a` and `x`'s
/// low bits; otherwise the doubles are added and converted (`@velt.to_int32`).
const ADD_INT32: &str = "define internal i32 @velt.add_int32(i32 %a, double %x) alwaysinline nounwind {
  %ax = call double @llvm.fabs.f64(double %x)
  %in = fcmp ole double %ax, 0x4330000000000000
  br i1 %in, label %conv, label %slow, !prof !{!\"branch_weights\", i32 2000, i32 1}
conv:
  %t = fptosi double %x to i64
  %back = sitofp i64 %t to double
  %whole = fcmp oeq double %back, %x
  br i1 %whole, label %int, label %slow, !prof !{!\"branch_weights\", i32 2000, i32 1}
int:
  %t32 = trunc i64 %t to i32
  %r = add i32 %a, %t32
  ret i32 %r
slow:
  %af = sitofp i32 %a to double
  %s = fadd double %af, %x
  %r2 = call i32 @velt.to_int32(double %s)
  ret i32 %r2
}";

/// The declaration `math_intrinsic` also emits for `Math.abs` (one line, so the two coincide).
const FABS: &str = "declare double @llvm.fabs.f64(double)";

/// `Math.clz32` (`velt_rt_math_clz32`): `ctlz`, defined for 0 (32).
const CLZ32: &str = "declare i32 @llvm.ctlz.i32(i32, i1)
define internal i32 @velt.clz32(i32 %x) alwaysinline nounwind {
  %r = call i32 @llvm.ctlz.i32(i32 %x, i1 false)
  ret i32 %r
}";

/// A runtime function the backend calls through a helper defined in the module instead: the
/// helper's name and its definition (with what it declares), for the signature
/// `(params) -> ret`.
pub(crate) fn inline_helper(symbol: &str) -> Option<InlineHelper> {
    Some(match symbol {
        "velt_rt_math_to_int32" => ("@velt.to_int32", &[TO_INT32], &[Ty::F64], Ty::I32),
        "velt_rt_math_mul_int32" => ("@velt.mul_int32", &[MUL_INT32], &[Ty::I32, Ty::I32], Ty::I32),
        "velt_rt_math_add_int32" => (
            "@velt.add_int32",
            &[FABS, ADD_INT32, TO_INT32],
            &[Ty::I32, Ty::F64],
            Ty::I32,
        ),
        "velt_rt_math_clz32" => ("@velt.clz32", &[CLZ32], &[Ty::I32], Ty::I32),
        _ => return None,
    })
}

/// A helper's name, its definitions (with what they declare), and its signature.
pub(crate) type InlineHelper = (&'static str, &'static [&'static str], &'static [Ty], Ty);

/// How much memory a runtime function may touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effects {
    /// Anything (the default for externs).
    Any,
    /// Reads memory, writes none (string comparison, hashing).
    ReadOnly,
    /// Touches no memory (arithmetic).
    None,
}

/// Memory effects of the runtime function `symbol`.
pub(crate) fn effects(symbol: &str) -> Effects {
    match symbol {
        "velt_rt_math_sqrt" | "velt_rt_math_floor" | "velt_rt_math_ceil" | "velt_rt_math_round"
        | "velt_rt_math_trunc" | "velt_rt_math_fabs" | "velt_rt_pow_f64" | "velt_rt_pow_i64"
        | "velt_rt_math_to_int32" | "velt_rt_math_clz32" | "velt_rt_math_mul_int32"
        | "velt_rt_math_add_int32" => {
            Effects::None
        }
        "velt_rt_str_cmp"
        | "velt_rt_str_eq"
        | "velt_rt_str_hash"
        | "velt_rt_str_char_code_at"
        | "velt_rt_str_index_of"
        | "velt_rt_str_last_index_of"
        | "velt_rt_str_includes"
        | "velt_rt_str_starts_with"
        | "velt_rt_str_ends_with" => Effects::ReadOnly,
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
