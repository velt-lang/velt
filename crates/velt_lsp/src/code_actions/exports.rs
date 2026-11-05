//! "Convert to a named export" for `export default` (#1916): drop `default` before a named
//! declaration (`export default function f` → `export function f`), or turn `export default f;`
//! into the export list `export { f };`. Velt accepts default exports; this is the refactor a
//! style that prefers named exports asks for (and the fix of the lint that will report them).
//! A file that reads the default would stop compiling, and the action edits one file: it isn't
//! offered when an open document or a file of the workspace folders does
//! (`Server::default_imported`, with [`default_specifiers`] and [`names_module`]).

use std::path::{Component, Path, PathBuf};

use velt_common::Span;
use velt_syntax::ast::{ItemKind, Module};

use super::Fix;
use crate::analysis::Analysis;

/// Title of the action.
pub const TITLE: &str = "Convert to a named export";

/// The specifiers `module` reads a default export through: `import x from`, `{ default as x }`,
/// `export { default } from`, and `import * as ns` and `export * as ns`, which give `ns.default`.
pub fn default_specifiers(module: &Module) -> Vec<String> {
    (module.items.iter())
        .filter_map(|item| match &item.kind {
            ItemKind::Import(imp)
                if !imp.from.is_empty()
                    && (imp.namespace.is_some()
                        || imp.names.iter().any(|n| n.name.name == "default")) =>
            {
                Some(imp.from.clone())
            }
            _ => None,
        })
        .collect()
}

/// Does relative specifier `spec` in the file at `importer` name the module at `target`?
pub fn names_module(importer: &Path, spec: &str, target: &Path) -> bool {
    let Some(dir) = importer.parent().filter(|_| spec.starts_with('.')) else {
        return false;
    };
    let mut base = PathBuf::new();
    for part in dir.join(spec).components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                base.pop();
            }
            part => base.push(part),
        }
    }
    let index = target.file_stem().is_some_and(|s| s == "index");
    base == target || base == target.with_extension("") || (index && target.parent() == Some(&base))
}

/// The action for each `export default` with a name in the document's range `lo..hi`.
pub fn fixes(analysis: &Analysis, lo: u32, hi: u32) -> Vec<Fix> {
    named_default_edits(&analysis.module().ast, analysis.text())
        .into_iter()
        .filter(|(item, _)| item.lo <= hi && lo <= item.hi)
        .map(|(_, edit)| Fix {
            title: TITLE.into(),
            edits: vec![edit],
            diagnostic: None,
            preferred: false,
        })
        .collect()
}

/// For each `export default` of `module` (source `text`) whose export has a name: the written
/// item's span and the edit making it a named export.
fn named_default_edits(module: &Module, text: &str) -> Vec<(Span, (Span, String))> {
    let mut out = vec![];
    for (i, item) in module.items.iter().enumerate() {
        let ItemKind::Import(imp) = &item.kind else {
            continue;
        };
        if !velt_syntax::written_default(imp) {
            continue;
        }
        let n = &imp.names[0];
        let Some(kw) = n.alias.as_ref().map(|a| a.span) else {
            continue;
        };
        if kw == item.span {
            // `export default function f`: the declaration before it; drop `default `.
            let Some(decl) = i.checked_sub(1).map(|p| module.items[p].span) else {
                continue;
            };
            let rest = &text[kw.hi as usize..];
            let ws = (rest.len() - rest.trim_start().len()) as u32;
            out.push((decl, (Span::new(kw.file, kw.lo, kw.hi + ws), String::new())));
        } else {
            // `export default f;` → `export { f };`.
            let span = Span::new(kw.file, kw.lo, n.name.span.hi);
            out.push((item.span, (span, format!("{{ {} }}", n.name.name))));
        }
    }
    out
}
