//! The edit that imports a name: added to an existing `import { … } from "<spec>"` of the
//! document, or as a new import statement after the last import (or at the top of the file, below
//! a leading header comment).

use velt_common::Span;
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
        let last = import.names.last().expect("ICE: names checked non-empty");
        let end = last.alias.as_ref().unwrap_or(&last.name).span.hi;
        return (Span::new(file, end, end), format!(", {name}"));
    }
    let statement = format!("import {{ {name} }} from \"{spec}\";\n");
    let text = analysis.text();
    match imports.last() {
        Some((item, _)) => {
            let at = line_end(text, item.span.hi.saturating_sub(1) as usize);
            let at_end = at == text.len() && !text.ends_with('\n');
            let new = if at_end {
                format!("\n{}", statement.trim_end())
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
                format!("{statement}\n")
            };
            (Span::new(file, at as u32, at as u32), new)
        }
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

/// Where a file without imports gets its first: after a leading `//` comment block that a blank
/// line separates from the code (a header), else at the top.
fn after_header(text: &str) -> usize {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            offset += line.len();
            continue;
        }
        return if trimmed.is_empty() && offset > 0 {
            offset + line.len()
        } else {
            0
        };
    }
    0
}
