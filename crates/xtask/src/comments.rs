//! Whether two versions of a Rust file differ only in their comments (doc comments included),
//! which compile to the same code: the planner checks such a change without the tests of the
//! crates depending on it, the goldens, or Windows and macOS.
//!
//! The comparison is token-level, not line-level: `//` inside a string literal (a test's Velt
//! source in a raw string) is part of the string, so editing it is a code change.

/// `src` with its comments removed and every run of whitespace and comments as one space.
/// String and character literals are kept exactly; an unterminated one runs to the end.
pub fn code_of(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut gap = false;
    let mut i = 0;
    while i < c.len() {
        if c[i].is_whitespace() {
            gap = true;
            i += 1;
        } else if c[i] == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            gap = true;
        } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
            i = block_comment_end(&c, i);
            gap = true;
        } else {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            let end = literal_end(&c, i).unwrap_or(i + 1);
            out.extend(&c[i..end]);
            i = end;
        }
    }
    out
}

/// Whether the two versions compile to the same code.
pub fn same_code(old: &str, new: &str) -> bool {
    code_of(old) == code_of(new)
}

/// The end of the (nested) block comment opening at `i`.
fn block_comment_end(c: &[char], mut i: usize) -> usize {
    let mut depth = 0;
    while i < c.len() {
        if c[i] == '/' && c.get(i + 1) == Some(&'*') {
            depth += 1;
            i += 2;
        } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    c.len()
}

/// The end of the string, raw string or character literal starting at `i` (with its `b`, `c`
/// or `r` prefix), or `None` when none starts there (a lifetime's `'` included).
fn literal_end(c: &[char], i: usize) -> Option<usize> {
    let ident = |ch: char| ch.is_alphanumeric() || ch == '_';
    if i > 0 && ident(c[i - 1]) {
        return None;
    }
    let mut j = i;
    if matches!(c[j], 'b' | 'c') {
        j += 1;
    }
    if c.get(j) == Some(&'r') {
        let mut hashes = 0;
        let mut k = j + 1;
        while c.get(k) == Some(&'#') {
            hashes += 1;
            k += 1;
        }
        if c.get(k) != Some(&'"') {
            return None;
        }
        k += 1;
        while k < c.len() {
            if c[k] == '"' && (1..=hashes).all(|h| c.get(k + h) == Some(&'#')) {
                return Some(k + 1 + hashes);
            }
            k += 1;
        }
        return Some(c.len());
    }
    match c.get(j) {
        Some('"') => Some(quoted_end(c, j, '"')),
        Some('\'') if c.get(j + 1) == Some(&'\\') => Some(quoted_end(c, j, '\'')),
        Some('\'') if c.get(j + 2) == Some(&'\'') => Some(j + 3),
        _ => None,
    }
}

/// The end of the literal opening with `quote` at `i`, whose `\` escapes the next character.
fn quoted_end(c: &[char], i: usize, quote: char) -> usize {
    let mut k = i + 1;
    while k < c.len() {
        match c[k] {
            '\\' => k += 2,
            ch if ch == quote => return k + 1,
            _ => k += 1,
        }
    }
    c.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_edits_keep_the_code() {
        let old =
            "//! Crate docs.\n\n/// Adds.\nfn add(a: i64, b: i64) -> i64 {\n    a + b // sum\n}\n";
        let new = "//! Crate docs, reworded\n//! over two lines.\n\n/// Adds two numbers\n/// (see §14.18).\nfn add(a: i64, b: i64) -> i64 {\n    /* nested /* block */ comment */\n    a + b\n}\n";
        assert!(same_code(old, new));
        assert!(same_code("let x = 1;", "let x = 1; // one\r\n"));
    }

    #[test]
    fn code_edits_are_seen() {
        assert!(!same_code("let x = 1; // a", "let x = 2; // a"));
        assert!(!same_code("a + b", "a+b"));
        assert!(!same_code("a/**/b", "ab"));
    }

    #[test]
    fn comment_markers_inside_literals_are_code() {
        assert!(!same_code(r#"let s = "a // b";"#, r#"let s = "a // c";"#));
        assert!(!same_code(
            r#"let s = "a \" // b";"#,
            r#"let s = "a \" // c";"#
        ));
        let raw = |comment: &str| format!("let src = r#\"\n  // {comment}\n  let x = \"1\";\n\"#;");
        assert!(!same_code(&raw("Velt comment"), &raw("changed")));
        assert!(!same_code("b\"/* x */\"", "b\"/* y */\""));
        assert!(!same_code("let c = '/'; // x", "let c = '*'; // x"));
        assert!(!same_code(r"let c = '\''; a", r"let c = '\''; b"));
        // A literal stays exact, whitespace included.
        assert!(!same_code("\"a  b\"", "\"a b\""));
    }

    #[test]
    fn lifetimes_are_not_character_literals() {
        let old = "fn f<'a>(x: &'a str) -> &'a str { x } // one";
        let new = "fn f<'a>(x: &'a str) -> &'a str { x } // two";
        assert!(same_code(old, new));
        assert!(!same_code(old, "fn f<'a>(x: &'a str) -> &'a str { y }"));
        // `r#` raw identifiers are not raw strings.
        assert!(same_code("let r#type = 1; // a", "let r#type = 1; // b"));
    }

    #[test]
    fn unterminated_literals_run_to_the_end() {
        assert!(!same_code("\"abc // x", "\"abc // y"));
        assert!(!same_code("r#\"abc // x", "r#\"abc // y"));
    }
}
