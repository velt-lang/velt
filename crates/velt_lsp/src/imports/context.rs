//! Where in an import the cursor is, read from the text (the statement being typed usually does
//! not parse): inside the braces of `import { … } from "<spec>"`, or inside a module specifier
//! string (`from "…"`, `import("…")`, `import "…"`). Comments and other strings are skipped.

use crate::text_scan::{in_code, is_ident_byte};

/// An import position.
#[derive(Debug, PartialEq, Eq)]
pub enum ImportContext {
    /// Inside the braces of `import { … } from "spec"` (or `export { … } from`).
    Names {
        /// The module specifier after the braces.
        spec: String,
        /// The names already listed (their imported names, not aliases), except the one at the
        /// cursor.
        listed: Vec<String>,
        /// `import type { … }`: types only.
        types_only: bool,
    },
    /// Inside a module specifier string.
    Specifier {
        /// The text between the opening quote and the cursor.
        typed: String,
        /// Byte range of the string's contents (opening quote excluded, up to the closing quote
        /// or the end of the line).
        lo: u32,
        hi: u32,
    },
}

/// The import context at byte `offset` of `text`; `word_start` is where the identifier ending at
/// the cursor starts.
pub fn at(text: &str, offset: usize, word_start: usize) -> Option<ImportContext> {
    specifier(text, offset).or_else(|| names(text, offset, word_start))
}

fn specifier(text: &str, offset: usize) -> Option<ImportContext> {
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[line_start..offset];
    let quote_at = line.rfind(['"', '\''])?;
    let quote = line.as_bytes()[quote_at];
    let before = line[..quote_at].trim_end();
    let opens = ends_with_word(before, "from")
        || ends_with_word(before, "import")
        || before
            .strip_suffix('(')
            .is_some_and(|b| ends_with_word(b.trim_end(), "import"));
    if !opens {
        return None;
    }
    // The quote opens a string that the cursor is in (not one in a comment or another string).
    if !in_code(text, line_start + quote_at) || in_code(text, offset) {
        return None;
    }
    let lo = line_start + quote_at + 1;
    let rest = &text[offset..];
    let len = rest
        .bytes()
        .position(|b| matches!(b, b'"' | b'\'' | b'\n' | b'\r') || b == quote)
        .unwrap_or(rest.len());
    Some(ImportContext::Specifier {
        typed: text[lo..offset].to_string(),
        lo: lo as u32,
        hi: (offset + len) as u32,
    })
}

fn names(text: &str, offset: usize, word_start: usize) -> Option<ImportContext> {
    let bytes = text.as_bytes();
    // Back over the names listed before the cursor to the `{`.
    let mut i = word_start;
    while i > 0 && is_list_byte(bytes[i - 1]) {
        i -= 1;
    }
    if i == 0 || bytes[i - 1] != b'{' {
        return None;
    }
    let open = i - 1;
    if !in_code(text, open) {
        return None;
    }
    let (keyword, before) = last_word(&text[..open]);
    let types_only = keyword == "type" && last_word(before).0 == "import";
    if !(types_only || keyword == "import" || keyword == "export") {
        return None;
    }
    // Forward over the names after the cursor to the `}`, then `from "spec"`.
    let mut j = offset;
    while j < bytes.len() && is_list_byte(bytes[j]) {
        j += 1;
    }
    if bytes.get(j) != Some(&b'}') {
        return None;
    }
    let after = text[j + 1..].trim_start();
    let after = after.strip_prefix("from")?.trim_start();
    let quote = *after
        .as_bytes()
        .first()
        .filter(|q| matches!(q, b'"' | b'\''))?;
    let spec = &after[1..];
    let spec = &spec[..spec.find(quote as char)?];
    if spec.contains('\n') {
        return None;
    }
    let word_end = offset
        + text[offset..]
            .bytes()
            .take_while(|b| is_ident_byte(*b))
            .count();
    let mut listed = listed_names(&text[open + 1..word_start]);
    listed.extend(listed_names(&text[word_end..j]));
    Some(ImportContext::Names {
        spec: spec.to_string(),
        listed,
        types_only,
    })
}

/// What may appear between the braces of an import besides the names: whitespace and commas.
fn is_list_byte(b: u8) -> bool {
    is_ident_byte(b) || b == b',' || b.is_ascii_whitespace()
}

/// The imported names of a list fragment (`a, type B, c as d` → `a`, `B`, `c`).
fn listed_names(list: &str) -> Vec<String> {
    list.split(',')
        .filter_map(|entry| {
            let mut words = entry.split_whitespace();
            let first = words.next()?;
            let name = if first == "type" {
                words.next().unwrap_or(first)
            } else {
                first
            };
            Some(name.to_string())
        })
        .collect()
}

/// The identifier `text` ends with (after trailing whitespace) and the text before it.
fn last_word(text: &str) -> (&str, &str) {
    let trimmed = text.trim_end();
    let start = trimmed
        .bytes()
        .rposition(|b| !is_ident_byte(b))
        .map_or(0, |i| i + 1);
    (&trimmed[start..], &trimmed[..start])
}

/// Whether `text` ends with the whole word `word`.
fn ends_with_word(text: &str, word: &str) -> bool {
    last_word(text).0 == word && text.trim_end().len() == text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The context at the `|` of `marked`.
    fn context(marked: &str) -> Option<ImportContext> {
        let offset = marked.find('|').unwrap();
        let text = marked.replacen('|', "", 1);
        let word_start = text[..offset]
            .bytes()
            .rposition(|b| !is_ident_byte(b))
            .map_or(0, |i| i + 1);
        at(&text, offset, word_start)
    }

    fn names(spec: &str, listed: &[&str], types_only: bool) -> Option<ImportContext> {
        Some(ImportContext::Names {
            spec: spec.into(),
            listed: listed.iter().map(|s| s.to_string()).collect(),
            types_only,
        })
    }

    #[test]
    fn names_inside_braces() {
        assert_eq!(
            context("import { a, re| } from \"velt:fs\";"),
            names("velt:fs", &["a"], false)
        );
        assert_eq!(
            context("import {\n  b as c,\n  |\n  type D,\n} from './m'"),
            names("./m", &["b", "D"], false)
        );
        assert_eq!(
            context("import type { | } from \"velt:fs\""),
            names("velt:fs", &[], true)
        );
        assert_eq!(
            context("import { größe, | } from \"./m\""),
            names("./m", &["größe"], false)
        );
        assert_eq!(context("function f() { a, | }"), None);
        assert_eq!(context("// import { a, | } from \"velt:fs\""), None);
        assert_eq!(context("/* import { | } from \"velt:fs\" */"), None);
        assert_eq!(context("import { a, |"), None);
        assert_eq!(context("const x = { a: 1, | } from"), None);
    }

    #[test]
    fn specifier_strings() {
        assert_eq!(
            context("import { a } from \"velt:c|\";"),
            Some(ImportContext::Specifier {
                typed: "velt:c".into(),
                lo: 19,
                hi: 25
            })
        );
        assert!(matches!(
            context("const m = await import('./|"),
            Some(ImportContext::Specifier { typed, .. }) if typed == "./"
        ));
        assert!(matches!(
            context("export * from \"|\""),
            Some(ImportContext::Specifier { typed, .. }) if typed.is_empty()
        ));
        assert_eq!(context("const s = \"from |\";"), None);
        assert_eq!(
            context("import x from \"./a|\r\nconst y = 1;"),
            Some(ImportContext::Specifier {
                typed: "./a".into(),
                lo: 15,
                hi: 18
            })
        );
        assert_eq!(context("// import x from \"./a|\""), None);
        assert_eq!(context("const s = \"x\" + ` from \"|`"), None);
        assert_eq!(context("console.log(\"|\")"), None);
        assert!(matches!(
            context("import x from \"😀|"),
            Some(ImportContext::Specifier { typed, .. }) if typed == "😀"
        ));
    }
}
