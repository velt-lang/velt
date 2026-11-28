//! Inline fast paths for the string functions short-lived strings call most (#531).
//!
//! A call to one of these functions is emitted as a call to an `internal alwaysinline` helper
//! defined in the module, which handles the common forms in a few instructions and calls the
//! function itself for the rest. Once inlined, LLVM sees the string's words, so temporaries stay
//! in registers and comparisons with literals fold to constant-size compares:
//! - `velt_rt_str_drop`: only a heap string (`(int64_t)w2 > 0`) has anything to release; static
//!   and inline strings are left as they are (they drop as nothing, zeroed or not).
//! - std's `strEquals` (`==`, std/lang/hooks/str_key.velt, `@noInline`): different byte lengths
//!   are unequal; equal lengths compare the bytes with `memcmp`, which LLVM expands inline when
//!   the length is a constant (a literal operand). The helper never calls `strEquals`.
//! - std's `strHash` (the same file): an inline string of at most 8 bytes (most `Map<string, V>`
//!   keys) hashes from the first word of the value, masked to its length; other strings call it.
//! - `velt_rt_str_cmp` against a one-byte literal: see `string_compare`.
//!
//! (`slice`'s one unit of an ASCII string, which was here, is std's own code now:
//! std/prelude/string.velt, #2198.)
//!
//! The layout is rt_abi.md "Strings": `{w0, w1, w2}`, inline when the top bit of `w2` is set
//! (byte length in bits 56..61, `0x40` of byte 23 set when non-ASCII, text in the value itself),
//! otherwise `{ptr, units << 32 | len, w2}`, where `w2` is 0 for a static string, the capacity
//! for a plain heap one, and the slice form's flags, offset and capacity for a slice: generated
//! code tests only `(int64_t)w2 > 0` (heap) and never reads `w2` as the capacity.

use velt_vir::vir::Ty;

/// Length and data address of the string at `%{p}`, as `%{p}.len` / `%{p}.data`, plus its words
/// `%{p}.w1`, `%{p}.w2` and the inline flag `%{p}.inl`.
pub(crate) fn view(p: &str) -> String {
    format!(
        "  %{p}.w1p = getelementptr inbounds i8, ptr %{p}, i64 8
  %{p}.w1 = load i64, ptr %{p}.w1p, align 8
  %{p}.w2p = getelementptr inbounds i8, ptr %{p}, i64 16
  %{p}.w2 = load i64, ptr %{p}.w2p, align 8
  %{p}.inl = icmp slt i64 %{p}.w2, 0
  %{p}.top = lshr i64 %{p}.w2, 56
  %{p}.short = and i64 %{p}.top, 31
  %{p}.lo = trunc i64 %{p}.w1 to i32
  %{p}.long = sext i32 %{p}.lo to i64
  %{p}.len = select i1 %{p}.inl, i64 %{p}.short, i64 %{p}.long
  %{p}.ptr = load ptr, ptr %{p}, align 8
  %{p}.data = select i1 %{p}.inl, ptr %{p}, ptr %{p}.ptr
"
    )
}

/// `velt_rt_str_drop(ptr)`.
fn drop_helper() -> String {
    "define internal void @velt.str_drop(ptr %s) alwaysinline nounwind {
  %w2p = getelementptr inbounds i8, ptr %s, i64 16
  %w2 = load i64, ptr %w2p, align 8
  %heap = icmp sgt i64 %w2, 0
  br i1 %heap, label %call, label %done
call:
  call void @velt_rt_str_drop(ptr %s)
  br label %done
done:
  ret void
}"
    .into()
}

/// `strEquals(ptr, ptr) -> bool` (an `i8`); `size_t` is `memcmp`'s length type (`i32` on
/// wasm32).
fn eq_helper(size_t: &str) -> String {
    let (trunc, n) = if size_t == "i64" {
        (String::new(), "%a.len")
    } else {
        (
            format!(
                "  %n = trunc i64 %a.len to {size_t}
"
            ),
            "%n",
        )
    };
    format!(
        "define internal zeroext i8 @velt.str_eq(ptr %a, ptr %b) alwaysinline nounwind {{
{}{}  %same = icmp eq i64 %a.len, %b.len
  br i1 %same, label %bytes, label %no
bytes:
{trunc}  %r = call i32 @memcmp(ptr %a.data, ptr %b.data, {size_t} {n})
  %eq = icmp eq i32 %r, 0
  %out = zext i1 %eq to i8
  ret i8 %out
no:
  ret i8 0
}}",
        view("a"),
        view("b")
    )
}

/// `strHash(ptr) -> u64` (`hook`, the function's LLVM name) for inline strings of at most 8
/// bytes, bit for bit std's (one word: the bytes zero-extended little-endian, `mix`, `fmix64`).
/// The word is masked to the string's length, so whatever an inline string holds after its text
/// does not matter.
fn hash_helper(hook: &str) -> String {
    // std/lang/hooks/str_key.velt's SEED and K, and murmur3's fmix64 constants.
    const SEED: u64 = 0x243f_6a88_85a3_08d3;
    const K: u64 = 0x9e37_79b9_7f4a_7c15;
    const C1: u64 = 0xff51_afd7_ed55_8ccd;
    const C2: u64 = 0xc4ce_b9fe_1a85_ec53;
    format!(
        "define internal i64 @velt.str_hash(ptr %s) alwaysinline nounwind {{
{}  %fits = icmp ule i64 %s.len, 8
  %fast = and i1 %s.inl, %fits
  br i1 %fast, label %inline, label %call
inline:
  %len.k = mul i64 %s.len, {K}
  %seed = xor i64 {SEED}, %len.k
  %nonempty = icmp ne i64 %s.len, 0
  br i1 %nonempty, label %mix, label %finish
mix:
  %word = load i64, ptr %s, align 8
  %bits = shl i64 %s.len, 3
  %unused = sub i64 64, %bits
  %mask = lshr i64 -1, %unused
  %text = and i64 %word, %mask
  %x = xor i64 %seed, %text
  %product = mul i64 %x, {K}
  %left = shl i64 %product, 29
  %right = lshr i64 %product, 35
  %mixed = or i64 %left, %right
  br label %finish
finish:
  %h = phi i64 [ %seed, %inline ], [ %mixed, %mix ]
  %h.33 = lshr i64 %h, 33
  %a = xor i64 %h, %h.33
  %b = mul i64 %a, {C1}
  %b.33 = lshr i64 %b, 33
  %c = xor i64 %b, %b.33
  %d = mul i64 %c, {C2}
  %d.33 = lshr i64 %d, 33
  %out = xor i64 %d, %d.33
  ret i64 %out
call:
  %result = call i64 {hook}(ptr %s)
  ret i64 %result
}}",
        view("s")
    )
}

/// The helper to call instead of the runtime string function `symbol` (of VIR signature
/// `params -> ret`), with the module-level definitions it needs; `None` for other functions (or
/// an unexpected signature, e.g. a user `declare` of the same name).
pub(crate) fn fast_path(
    symbol: &str,
    params: &[Ty],
    ret: Ty,
) -> Option<(&'static str, Vec<String>)> {
    use Ty::{Ptr, Unit};
    match (symbol, params, ret) {
        ("velt_rt_str_drop", [Ptr], Unit) => Some(("@velt.str_drop", vec![drop_helper()])),
        _ => None,
    }
}

/// The mangled symbols of std's `strEquals` and `strHash` (velt_common `std_hooks.rs`).
fn hook_symbols() -> &'static (String, String) {
    static SYMBOLS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    SYMBOLS.get_or_init(|| {
        (
            velt_vir::mangle::mangle(velt_common::std_hook!(str_equals)),
            velt_vir::mangle::mangle(velt_common::std_hook!(str_hash)),
        )
    })
}

/// The helper to call instead of the std function `symbol` (of VIR signature `params -> ret`)
/// when it is `strEquals` or `strHash`, with the module-level definitions it needs; `None` for
/// other functions. `ptr32`: the target's pointers and `size_t` are 32 bits (wasm32).
pub(crate) fn hook_fast_path(
    symbol: &str,
    params: &[Ty],
    ret: Ty,
    ptr32: bool,
) -> Option<(&'static str, Vec<String>)> {
    use Ty::{Bool, Ptr, U64};
    let (eq, hash) = hook_symbols();
    let size_t = if ptr32 { "i32" } else { "i64" };
    match (params, ret) {
        ([Ptr, Ptr], Bool) if symbol == eq => Some((
            "@velt.str_eq",
            vec![
                eq_helper(size_t),
                format!(
                    "declare i32 @memcmp(ptr, ptr, {size_t}) nounwind willreturn memory(argmem: read)"
                ),
            ],
        )),
        ([Ptr], U64) if symbol == hash => Some((
            "@velt.str_hash",
            vec![hash_helper(&crate::types::global_name(symbol))],
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_expected_signatures_take_a_fast_path() {
        assert!(fast_path("velt_rt_str_drop", &[Ty::Ptr], Ty::Unit).is_some());
        assert!(fast_path("velt_rt_str_drop", &[Ty::Ptr, Ty::Ptr], Ty::Unit).is_none());
        // std's `slice` makes one unit of an ASCII string itself (std/prelude/string.velt, #2198).
        let slice = [Ty::Ptr, Ty::U64, Ty::U64, Ty::U64, Ty::Ptr];
        assert!(fast_path("velt_rt_str_byte_slice", &slice, Ty::Unit).is_none());
        assert!(fast_path("velt_rt_str_concat", &[Ty::Ptr, Ty::Ptr, Ty::Ptr], Ty::Unit).is_none());
        // std's `==` and hash, by their mangled names, with their signatures only.
        let (eq, hash) = hook_symbols();
        assert_eq!(eq, "_V3stdP4langP5hooksP8str__keyN9strEquals");
        assert_eq!(hash, "_V3stdP4langP5hooksP8str__keyN7strHash");
        assert!(hook_fast_path(eq, &[Ty::Ptr, Ty::Ptr], Ty::Bool, false).is_some());
        assert!(hook_fast_path(eq, &[Ty::Ptr], Ty::Bool, false).is_none());
        assert!(hook_fast_path(hash, &[Ty::Ptr], Ty::U64, false).is_some());
        assert!(hook_fast_path(hash, &[Ty::Ptr], Ty::I64, false).is_none());
        assert!(hook_fast_path("_V4main", &[Ty::Ptr], Ty::U64, false).is_none());
    }

    #[test]
    fn hash_masks_the_inline_word_to_the_length() {
        let hash = &hook_symbols().1;
        let (name, defs) = hook_fast_path(hash, &[Ty::Ptr], Ty::U64, false).unwrap();
        assert_eq!(name, "@velt.str_hash");
        let ir = &defs[0];
        // Only inline strings of at most 8 bytes; everything else calls std's function.
        assert!(ir.contains("%fits = icmp ule i64 %s.len, 8"), "{ir}");
        assert!(ir.contains("%fast = and i1 %s.inl, %fits"), "{ir}");
        assert!(
            ir.contains(&format!("call i64 @\"{hash}\"(ptr %s)")),
            "{ir}"
        );
        // The word is read only for a non-empty string and masked to its bytes.
        let mix = &ir[ir.find("\nmix:").expect("mix block")..];
        assert!(mix.contains("%mask = lshr i64 -1, %unused"), "{ir}");
        assert!(mix.contains("%x = xor i64 %seed, %text"), "{ir}");
        // std's seed, multiplier and rotation.
        assert!(
            ir.contains(&format!("xor i64 {}", 0x243f_6a88_85a3_08d3u64)),
            "{ir}"
        );
        assert!(
            ir.contains(&format!("mul i64 %x, {}", 0x9e37_79b9_7f4a_7c15u64)),
            "{ir}"
        );
        assert!(ir.contains("shl i64 %product, 29") && ir.contains("lshr i64 %product, 35"));
        // The empty string hashes the seed alone.
        assert!(
            ir.contains("%h = phi i64 [ %seed, %inline ], [ %mixed, %mix ]"),
            "{ir}"
        );
    }

    #[test]
    fn memcmp_takes_the_targets_size_t() {
        let sym = &hook_symbols().0;
        let eq = |ptr32| hook_fast_path(sym, &[Ty::Ptr, Ty::Ptr], Ty::Bool, ptr32).unwrap();
        let (_, defs) = eq(true);
        assert!(defs[0].contains("@memcmp(ptr %a.data, ptr %b.data, i32 %n)"));
        assert!(defs[1].contains("@memcmp(ptr, ptr, i32)"));
        let (_, defs) = eq(false);
        assert!(defs[0].contains("@memcmp(ptr %a.data, ptr %b.data, i64 %a.len)"));
        assert!(defs[1].contains("@memcmp(ptr, ptr, i64)"));
    }
}
