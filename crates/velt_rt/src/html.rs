//! HTML escaping for `std/html` (`escapeHtml`) and `std/jsx`: one pass over the bytes into a
//! buffer sized once, instead of the five `replaceAll` passes (five allocations) a Velt
//! implementation needs. Only ASCII bytes are rewritten, so UTF-8 sequences pass through
//! untouched. The two differ only in the apostrophe: `&#39;` for `escapeHtml` (lodash, the
//! TechEmpower fortunes test), `&#x27;` for std/jsx (react-dom).

use crate::str::VeltStr;

/// `escapeHtml`'s apostrophe.
const APOS_HTML: &[u8] = b"&#39;";
/// react-dom's apostrophe (std/jsx).
const APOS_REACT: &[u8] = b"&#x27;";

/// The entity for a byte that must be escaped in text and attribute values, with `apos` for
/// `'`, or `None`.
fn entity(b: u8, apos: &'static [u8]) -> Option<&'static [u8]> {
    match b {
        b'&' => Some(b"&amp;"),
        b'<' => Some(b"&lt;"),
        b'>' => Some(b"&gt;"),
        b'"' => Some(b"&quot;"),
        b'\'' => Some(apos),
        _ => None,
    }
}

/// The index of the first byte to escape.
fn first_entity(s: &[u8]) -> Option<usize> {
    s.iter()
        .position(|&b| matches!(b, b'&' | b'<' | b'>' | b'"' | b'\''))
}

/// `s` with `& < > " '` replaced by their entities (`&#39;` for `'`).
pub fn escape(s: &[u8]) -> Vec<u8> {
    match first_entity(s) {
        Some(first) => escape_from(s, first, APOS_HTML),
        None => s.to_vec(),
    }
}

/// `s`, whose first byte to escape is at `first`, with the entities written.
fn escape_from(s: &[u8], first: usize, apos: &'static [u8]) -> Vec<u8> {
    let extra: usize = s[first..]
        .iter()
        .filter_map(|&b| entity(b, apos))
        .map(|e| e.len() - 1)
        .sum();
    let mut out = Vec::with_capacity(s.len() + extra);
    out.extend_from_slice(&s[..first]);
    let mut start = first;
    for (i, &b) in s.iter().enumerate().skip(first) {
        if let Some(e) = entity(b, apos) {
            out.extend_from_slice(&s[start..i]);
            out.extend_from_slice(e);
            start = i + 1;
        }
    }
    out.extend_from_slice(&s[start..]);
    out
}

/// Escapes `*s` into `*out` with `apos` for `'`. Text with nothing to escape, the common case,
/// is `s` itself: a refcount increment instead of a copy.
unsafe fn escape_into(s: *const VeltStr, out: *mut VeltStr, apos: &'static [u8]) {
    let bytes = (*s).as_bytes();
    match first_entity(bytes) {
        Some(first) => out.write(VeltStr::from_vec(escape_from(bytes, first, apos))),
        None => crate::str::velt_rt_str_own(s, out),
    }
}

/// `escapeHtml(s)` (`'` → `&#39;`) into an owned string (`s` is only read).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_html_escape(s: *const VeltStr, out: *mut VeltStr) {
    escape_into(s, out, APOS_HTML);
}

/// std/jsx's escaping of text and attribute values: as `velt_rt_html_escape`, but `'` →
/// `&#x27;`, as react-dom writes it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_jsx_escape(s: *const VeltStr, out: *mut VeltStr) {
    escape_into(s, out, APOS_REACT);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(s: &str) -> String {
        String::from_utf8(escape(s.as_bytes())).unwrap()
    }

    #[test]
    fn jsx_escape_writes_react_doms_apostrophe() {
        unsafe {
            let s = VeltStr::from_text("it's \"q\" & <x>");
            let mut out = std::mem::MaybeUninit::<VeltStr>::uninit();
            velt_rt_jsx_escape(&s, out.as_mut_ptr());
            let mut out = out.assume_init();
            assert_eq!(out.as_bytes(), b"it&#x27;s &quot;q&quot; &amp; &lt;x&gt;");
            out.release();
        }
    }

    #[test]
    fn text_without_entities_is_shared_not_copied() {
        unsafe {
            let s = VeltStr::from_text("a heap string longer than the inline limit");
            let mut out = std::mem::MaybeUninit::<VeltStr>::uninit();
            velt_rt_html_escape(&s, out.as_mut_ptr());
            let mut out = out.assume_init();
            assert!(out.is_heap());
            assert_eq!(out.as_bytes().as_ptr(), s.as_bytes().as_ptr());
            out.release();
        }
    }

    #[test]
    fn escapes_the_five_characters_in_one_pass() {
        assert_eq!(esc(""), "");
        assert_eq!(esc("plain — ベンチマーク"), "plain — ベンチマーク");
        assert_eq!(
            esc(r#"<script>alert("x & 'y'");</script>"#),
            "&lt;script&gt;alert(&quot;x &amp; &#39;y&#39;&quot;);&lt;/script&gt;"
        );
        assert_eq!(esc("&&"), "&amp;&amp;");
        assert_eq!(esc("a<"), "a&lt;");
        // Already-escaped text is escaped again, like the replaceAll chain it replaces.
        assert_eq!(esc("&amp;"), "&amp;amp;");
    }

    #[test]
    fn abi_writes_an_owned_string() {
        let s = VeltStr::from_static(b"1 > 0");
        let mut out = std::mem::MaybeUninit::uninit();
        unsafe {
            velt_rt_html_escape(&s, out.as_mut_ptr());
            let mut out = out.assume_init();
            assert_eq!(out.as_bytes(), b"1 &gt; 0");
            crate::str::velt_rt_str_drop(&mut out);
        }
    }
}
