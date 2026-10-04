//! The edit that imports a name: added to an existing `import { … } from "<spec>"` of the
//! document (in order when its names are sorted), or as a new import statement after the last
//! import (or at the top of the file, below a leading header comment). New lines use the
//! document's line ending.

use velt_common::{FileId, Span};
use velt_syntax::ast;

use crate::analysis::Analysis;

/// The edit importing `name` from `spec` into the document (`is_type`: `name` is a type, which
/// an `import type { … }` may take).
pub fn import_edit(analysis: &Analysis, spec: &str, name: &str, is_type: bool) -> (Span, String) {
    let file = analysis.file();
    let module = &analysis.module().ast;
    let imports: Vec<(&ast::Item, &ast::Import)> = module
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ast::ItemKind::Import(i) if !i.from.is_empty() => Some((item, i)),
            _ => None,
        })
        .collect();
    let extendable = imports.iter().find(|(item, i)| {
        !item.exported
            && i.from == spec
            && i.namespace.is_none()
            && !i.names.is_empty()
            && (is_type || !is_type_only(analysis, item))
    });
    if let Some((_, import)) = extendable {
        return into_list(analysis.text(), file, import, name);
    }
    let text = analysis.text();
    let nl = newline(text);
    let statement = format!("import {{ {name} }} from \"{spec}\";{nl}");
    match imports.last() {
        Some((item, _)) => {
            let at = line_end(text, item.span.hi.saturating_sub(1) as usize);
            let new = if at == text.len() && !text.ends_with('\n') {
                format!("{nl}{}", statement.trim_end())
            } else {
                statement
            };
            (Span::new(file, at as u32, at as u32), new)
        }
        None => {
            let at = after_header(text);
            let rest = &text[at..];
            // Keep a blank line between the imports and the code.
            let new = if rest.is_empty() || rest.starts_with('\n') || rest.starts_with("\r\n") {
                statement
            } else {
                format!("{statement}{nl}")
            };
            (Span::new(file, at as u32, at as u32), new)
        }
    }
}

/// `name` added to the names of `import`: in its place when they are sorted (ignoring case),
/// else at the end.
fn into_list(text: &str, file: FileId, import: &ast::Import, name: &str) -> (Span, String) {
    let key = |n: &ast::ImportName| n.name.name.to_lowercase();
    let sorted = import.names.windows(2).all(|w| key(&w[0]) <= key(&w[1]));
    let lower = name.to_lowercase();
    let before = sorted
        .then(|| import.names.iter().find(|n| key(n) > lower))
        .flatten();
    if let Some(next) = before {
        // `{ type B }` starts at `type`; the name's span is after it.
        let lo = next.name.span.lo as usize;
        let before = text.get(..lo).unwrap_or("").trim_end();
        let at = match before.strip_suffix("type") {
            Some(rest) if rest.ends_with([' ', '\t', '\n', ',', '{']) => rest.len(),
            _ => lo,
        };
        let at = at as u32;
        return (Span::new(file, at, at), format!("{name}, "));
    }
    let last = import.names.last().expect("ICE: names checked non-empty");
    let end = last.alias.as_ref().unwrap_or(&last.name).span.hi;
    (Span::new(file, end, end), format!(", {name}"))
}

/// The document's line ending (`\r\n` if its first line ends so).
fn newline(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

/// Whether `item` is an `import type { … }` (only types may be added to it).
fn is_type_only(analysis: &Analysis, item: &ast::Item) -> bool {
    let text = analysis.snippet(item.span);
    let rest = text.trim_start().strip_prefix("import").unwrap_or("");
    rest.trim_start()
        .strip_prefix("type")
        .is_some_and(|after| after.starts_with(|c: char| c.is_whitespace() || c == '{'))
}

/// The offset just past the line break ending the line that contains `offset` (the text's end
/// if there is none).
fn line_end(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    text[offset..]
        .find('\n')
        .map_or(text.len(), |i| offset + i + 1)
}

/// Where a file without imports gets its first: after the comments it starts with (`//` lines,
/// `/* … */` and `/** … */` blocks) when a blank line separates them from the code (a header),
/// else at the top.
fn after_header(text: &str) -> usize {
    let mut at = 0;
    loop {
        let rest = &text[at..];
        let line = rest.trim_start_matches([' ', '\t']);
        let end = if line.starts_with("//") {
            line_end(text, at)
        } else if line.starts_with("/*") {
            match rest.find("*/") {
                Some(close) => line_end(text, at + close + 2),
                None => return 0,
            }
        } else {
            break;
        };
        at = end;
    }
    if at == 0 {
        return 0;
    }
    let rest = &text[at..];
    let blank = rest.find('\n').filter(|&i| rest[..i].trim().is_empty());
    match blank {
        Some(i) => at + i + 1,
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_comment_blocks_before_a_blank_line() {
        assert_eq!(after_header("// a\n// b\n\ncode"), 11);
        assert_eq!(after_header("/**\n * License.\n */\n\ncode"), 21);
        assert_eq!(after_header("/* a */\r\n\r\ncode"), 11);
        assert_eq!(after_header("/** Doc. */\nfunction f() {}"), 0);
        assert_eq!(after_header("code"), 0);
        assert_eq!(after_header("/* open"), 0);
    }

    #[test]
    fn newlines_follow_the_document() {
        assert_eq!(newline("a\r\nb"), "\r\n");
        assert_eq!(newline("a\nb"), "\n");
        assert_eq!(newline(""), "\n");
    }
}
