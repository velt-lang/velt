//! Compile-time HTML escaping for the precompile lowering: the character set of `std/html`
//! `escapeHtml` (`velt_rt_html_escape`), so static template text renders byte-identical to a
//! runtime that escapes the same text with it.

/// `s` with `&`, `<`, `>`, `"` and `'` replaced by `&amp;`, `&lt;`, `&gt;`, `&quot;` and `&#39;`.
pub(super) fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape_html;

    #[test]
    fn escapes_like_escape_html() {
        assert_eq!(
            escape_html("<script>alert(\"x & 'y'\");</script>"),
            "&lt;script&gt;alert(&quot;x &amp; &#39;y&#39;&quot;);&lt;/script&gt;"
        );
        assert_eq!(escape_html("héllo — ✓"), "héllo — ✓");
        assert_eq!(escape_html(""), "");
    }
}
