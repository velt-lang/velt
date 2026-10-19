//! Compile-time HTML escaping for the precompile lowering: `& < > "`, the entities every
//! provider writes (std/jsx, react-dom, sigx, `escapeHtml`). They differ for `'` (`&#x27;`,
//! `&#39;`), so static text and attribute values containing one are escaped by the provider at
//! run time (`jsxEscape`, `jsxAttr`) and never reach this function.

/// `s` (without `'`) with `&`, `<`, `>` and `"` replaced by `&amp;`, `&lt;`, `&gt;` and `&quot;`.
pub(super) fn escape_html(s: &str) -> String {
    debug_assert!(!s.contains('\''), "ICE: `'` is escaped by the provider");
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
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
            escape_html("<script>alert(\"x & y\");</script>"),
            "&lt;script&gt;alert(&quot;x &amp; y&quot;);&lt;/script&gt;"
        );
        assert_eq!(escape_html("héllo — ✓"), "héllo — ✓");
        assert_eq!(escape_html(""), "");
    }
}
