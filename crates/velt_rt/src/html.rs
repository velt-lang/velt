//! HTML escaping for `std/html` (`escapeHtml`): one pass over the bytes into a buffer sized
//! once, instead of the five `replaceAll` passes (five allocations) a Velt implementation needs.
//! Only ASCII bytes are rewritten, so UTF-8 sequences pass through untouched.

use crate::str::VeltStr;

/// The entity for a byte that must be escaped in text and attribute values (the set React,
/// lodash and the TechEmpower fortunes test use), or `None`.
fn entity(b: u8) -> Option<&'static [u8]> {
    match b {
        b'&' => Some(b"&amp;"),
        b'<' => Some(b"&lt;"),
        b'>' => Some(b"&gt;"),
        b'"' => Some(b"&quot;"),
        b'\'' => Some(b"&#39;"),
        _ => None,
    }
}

/// `s` with `& < > " '` replaced by their entities.
pub fn escape(s: &[u8]) -> Vec<u8> {
    let Some(first) = s.iter().position(|&b| entity(b).is_some()) else {
        return s.to_vec();
    };
    let extra: usize = s[first..]
        .iter()
        .filter_map(|&b| entity(b))
        .map(|e| e.len() - 1)
        .sum();
    let mut out = Vec::with_capacity(s.len() + extra);
    out.extend_from_slice(&s[..first]);
    let mut start = first;
    for (i, &b) in s.iter().enumerate().skip(first) {
        if let Some(e) = entity(b) {
            out.extend_from_slice(&s[start..i]);
            out.extend_from_slice(e);
            start = i + 1;
        }
    }
    out.extend_from_slice(&s[start..]);
    out
}

/// `escapeHtml(s)` into a new owned string (`s` is only read).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_html_escape(s: *const VeltStr, out: *mut VeltStr) {
    out.write(VeltStr::from_vec(escape((*s).as_bytes())));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(s: &str) -> String {
        String::from_utf8(escape(s.as_bytes())).unwrap()
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
