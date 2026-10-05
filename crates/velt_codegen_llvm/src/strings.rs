//! Inline fast paths for the string runtime functions short-lived strings call most (#531).
//!
//! A call to one of these externs is emitted as a call to an `internal alwaysinline` helper
//! defined in the module, which handles the common forms in a few instructions and calls the
//! runtime function for the rest. Once inlined, LLVM sees the string's words, so temporaries stay
//! in registers and comparisons with literals fold to constant-size compares:
//! - `velt_rt_str_drop`: only a heap string (`(int64_t)w2 > 0`) has anything to release; static
//!   and inline strings are left as they are (they drop as nothing, zeroed or not).
//! - `velt_rt_str_eq`: different byte lengths are unequal; equal lengths compare the bytes with
//!   `memcmp`, which LLVM expands inline when the length is a constant (a literal operand).
//! - `velt_rt_str_slice(s, i, i + 1)` (`s[i]`, `charAt`, `slice`) of an ASCII string (unit count
//!   == byte count, so positions mean the same in bytes and code units) with `0 <= i < len`:
//!   the one-byte inline string `{s[i], 0, (0x80 | 1) << 56}`.
//!
//! The layout is rt_abi.md "Strings": `{w0, w1, w2}`, inline when the top bit of `w2` is set
//! (byte length in bits 56..61, `0x40` of byte 23 set when non-ASCII, text in the value itself),
//! otherwise `{ptr, units << 32 | len, cap}` (`cap == 0`: static).

use velt_vir::vir::Ty;

/// Length and data address of the string at `%{p}`, as `%{p}.len` / `%{p}.data`, plus its words
/// `%{p}.w1`, `%{p}.w2` and the inline flag `%{p}.inl`.
fn view(p: &str) -> String {
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

/// `velt_rt_str_eq(ptr, ptr) -> u8`; `size_t` is `memcmp`'s length type (`i32` on wasm32).
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

/// `w2` of a one-byte inline ASCII string, `(0x80 | 1) << 56`, as an `i64`.
const INLINE_ONE_W2: i64 = (0x81u64 << 56) as i64;

/// `velt_rt_str_slice(ptr s, i64 start, i64 end, ptr out)`.
fn slice_helper() -> String {
    format!(
        "define internal void @velt.str_slice(ptr %s, i64 %start, i64 %end, ptr %out) alwaysinline nounwind {{
{}  %units = lshr i64 %s.w1, 32
  %bytes = and i64 %s.w1, 4294967295
  %heap.ascii = icmp eq i64 %units, %bytes
  %form = and i64 %s.top, 192
  %inl.ascii = icmp eq i64 %form, 128
  %ascii = select i1 %s.inl, i1 %inl.ascii, i1 %heap.ascii
  %inside = icmp ult i64 %start, %s.len
  %next = add i64 %start, 1
  %one = icmp eq i64 %end, %next
  %ok = and i1 %ascii, %inside
  %fast = and i1 %ok, %one
  br i1 %fast, label %byte, label %call
byte:
  %at = getelementptr inbounds i8, ptr %s.data, i64 %start
  %c = load i8, ptr %at, align 1
  %w0 = zext i8 %c to i64
  store i64 %w0, ptr %out, align 8
  %o1 = getelementptr inbounds i8, ptr %out, i64 8
  store i64 0, ptr %o1, align 8
  %o2 = getelementptr inbounds i8, ptr %out, i64 16
  store i64 {INLINE_ONE_W2}, ptr %o2, align 8
  ret void
call:
  call void @velt_rt_str_slice(ptr %s, i64 %start, i64 %end, ptr %out)
  ret void
}}",
        view("s")
    )
}

/// The helper to call instead of the runtime string function `symbol` (of VIR signature
/// `params -> ret`), with the module-level definitions it needs; `None` for other functions (or
/// an unexpected signature, e.g. a user `declare` of the same name). `ptr32`: the target's
/// pointers and `size_t` are 32 bits (wasm32).
pub(crate) fn fast_path(
    symbol: &str,
    params: &[Ty],
    ret: Ty,
    ptr32: bool,
) -> Option<(&'static str, Vec<String>)> {
    use Ty::{Ptr, Unit, I64, U8};
    let size_t = if ptr32 { "i32" } else { "i64" };
    let (name, defs) = match (symbol, params, ret) {
        ("velt_rt_str_drop", [Ptr], Unit) => ("@velt.str_drop", vec![drop_helper()]),
        ("velt_rt_str_eq", [Ptr, Ptr], U8) => (
            "@velt.str_eq",
            vec![
                eq_helper(size_t),
                format!(
                    "declare i32 @memcmp(ptr, ptr, {size_t}) nounwind willreturn memory(argmem: read)"
                ),
            ],
        ),
        ("velt_rt_str_slice", [Ptr, I64, I64, Ptr], Unit) => {
            ("@velt.str_slice", vec![slice_helper()])
        }
        _ => return None,
    };
    Some((name, defs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_expected_signatures_take_a_fast_path() {
        assert!(fast_path("velt_rt_str_drop", &[Ty::Ptr], Ty::Unit, false).is_some());
        assert!(fast_path("velt_rt_str_drop", &[Ty::Ptr, Ty::Ptr], Ty::Unit, false).is_none());
        assert!(fast_path("velt_rt_str_eq", &[Ty::Ptr, Ty::Ptr], Ty::U8, false).is_some());
        assert!(fast_path("velt_rt_str_cmp", &[Ty::Ptr, Ty::Ptr], Ty::I32, false).is_none());
        let slice = [Ty::Ptr, Ty::I64, Ty::I64, Ty::Ptr];
        assert!(fast_path("velt_rt_str_slice", &slice, Ty::Unit, false).is_some());
        assert!(fast_path(
            "velt_rt_str_concat",
            &[Ty::Ptr, Ty::Ptr, Ty::Ptr],
            Ty::Unit,
            false
        )
        .is_none());
    }

    #[test]
    fn memcmp_takes_the_targets_size_t() {
        let eq = |ptr32| fast_path("velt_rt_str_eq", &[Ty::Ptr, Ty::Ptr], Ty::U8, ptr32).unwrap();
        let (_, defs) = eq(true);
        assert!(defs[0].contains("@memcmp(ptr %a.data, ptr %b.data, i32 %n)"));
        assert!(defs[1].contains("@memcmp(ptr, ptr, i32)"));
        let (_, defs) = eq(false);
        assert!(defs[0].contains("@memcmp(ptr %a.data, ptr %b.data, i64 %a.len)"));
        assert!(defs[1].contains("@memcmp(ptr, ptr, i64)"));
    }

    #[test]
    fn the_inline_one_byte_tag_is_0x81() {
        assert_eq!((INLINE_ONE_W2 as u64) >> 56, 0x81);
        assert_eq!(INLINE_ONE_W2 as u64 & ((1 << 56) - 1), 0);
    }
}
