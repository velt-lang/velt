//! `jsx-pragma-comment`: a `@jsxImportSource` pragma in a line comment. Velt reads the pragma
//! from any comment before the first token; `tsc` only from a block comment, so a client
//! building the file with `tsc` silently uses its tsconfig's provider instead.

use velt_common::Span;
use velt_syntax::visit;

use super::imports::FirstJsx;
use super::Cx;
use crate::{Fix, LintModule};

const PRAGMA: &str = "@jsxImportSource";

/// Report the module's pragma if Velt honours it (the module has JSX, whose runtime it names)
/// and it is a line comment.
pub(super) fn check(module: &LintModule, cx: &mut Cx) {
    let Some(source) = module.ast.jsx_import_source.as_deref() else {
        return;
    };
    let mut first = FirstJsx(None);
    visit::walk_module(module.ast, &mut first);
    if first.0.is_none() {
        // Velt loads no runtime for a module without JSX, so the pragma says nothing.
        return;
    }
    let Some(comment) = pragma_comment(module.src, source) else {
        return;
    };
    let text = &module.src[comment.clone()];
    let Some(body) = text.strip_prefix("//") else {
        return;
    };
    let span = Span::new(
        module.ast.span.file,
        comment.start as u32,
        comment.end as u32,
    );
    let block = format!("/** {PRAGMA} {source} */");
    let write = format!("write it as a block comment, `{block}`, which both read");
    let notes = [
        "`tsc` reads `jsxImportSource` only from a block comment, so a client compiling this \
         file uses its tsconfig's provider, and the two sides render with different providers \
         without an error",
        write.as_str(),
    ];
    let message = format!("`tsc` ignores a `{PRAGMA}` pragma in a line comment");
    // A comment holding more than the pragma keeps its text: no mechanical fix.
    if body.trim_start_matches('/').trim() == format!("{PRAGMA} {source}") {
        let fix = Fix {
            span,
            replacement: block.clone(),
            title: format!("replace with `{block}`"),
        };
        cx.error_with_fix("jsx-pragma-comment", span, message, &notes, fix);
    } else {
        cx.error("jsx-pragma-comment", span, message, &notes);
    }
}

/// The byte range of the comment Velt read `source` from: the first comment before the first
/// token holding a pragma, read as the lexer reads it (velt_syntax `leading_pragma`).
fn pragma_comment(src: &str, source: &str) -> Option<std::ops::Range<usize>> {
    let bytes = src.as_bytes();
    let mut pos = 0;
    loop {
        if matches!(
            bytes.get(pos),
            Some(b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
        ) {
            pos += 1;
            continue;
        }
        match bytes.get(pos..pos + 2) {
            Some(b"//") => {
                let end = src[pos..].find('\n').map_or(src.len(), |i| pos + i);
                // The line break stays, `\r\n` included.
                let end = if src[..end].ends_with('\r') {
                    end - 1
                } else {
                    end
                };
                if let Some(found) = pragma_value(&src[pos..end]) {
                    return (found == source).then_some(pos..end);
                }
                pos = end;
            }
            Some(b"/*") => {
                let end = src[pos + 2..].find("*/").map(|i| pos + 2 + i + 2)?;
                if let Some(found) = pragma_value(&src[pos..end]) {
                    return (found == source).then_some(pos..end);
                }
                pos = end;
            }
            _ => return None,
        }
    }
}

/// The source a comment names, as the lexer reads it: the word after the first
/// `@jsxImportSource` (which must be followed by whitespace), up to whitespace or `*`.
fn pragma_value(comment: &str) -> Option<&str> {
    let rest = &comment[comment.find(PRAGMA)? + PRAGMA.len()..];
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let value = rest.trim_start();
    let end = value
        .find(|c: char| c.is_whitespace() || c == '*')
        .unwrap_or(value.len());
    Some(&value[..end]).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_comment_the_lexer_reads() {
        let src = "// header\n/* @jsxImportSourcex no */\n// @jsxImportSource ./ui\nconst a = 1;";
        let at = src.find("// @").unwrap();
        assert_eq!(pragma_comment(src, "./ui"), Some(at..at + 24));
        let src = "/** @jsxImportSource ./ui */\nconst a = 1;";
        assert_eq!(pragma_comment(src, "./ui"), Some(0..28));
        // After the first token, comments are not pragmas.
        assert_eq!(
            pragma_comment("const a = 1; // @jsxImportSource x", "x"),
            None
        );
        assert_eq!(
            pragma_comment("/* unterminated @jsxImportSource x", "x"),
            None
        );
    }
}
