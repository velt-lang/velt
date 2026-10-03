//! A script (a root file with top-level statements): the parser moved its statements into a
//! generated `main` whose name has an empty span. The file is printed as written, with the
//! statements back at the top level among the declarations, in source order.

use velt_syntax::ast::{Item, ItemKind, Module, Stmt};

use super::items::{blank_between, is_compact};

/// One top-level entry of a script.
pub(super) enum Top<'m> {
    Item(&'m Item),
    Stmt(&'m Stmt),
}

impl Top<'_> {
    pub(super) fn range(&self) -> (u32, u32) {
        match self {
            Top::Item(i) => (i.span.lo, i.span.hi),
            Top::Stmt(s) => (s.span.lo, s.span.hi),
        }
    }
}

/// The module's entries in source order, statements of a generated `main` included.
pub(super) fn entries(module: &Module) -> Vec<Top<'_>> {
    let mut out = vec![];
    for item in &module.items {
        match &item.kind {
            ItemKind::Function(f) if f.sig.name.span.lo == f.sig.name.span.hi => {
                out.extend(f.body.stmts.iter().map(Top::Stmt));
            }
            _ => out.push(Top::Item(item)),
        }
    }
    out.sort_by_key(|t| t.range().0);
    out
}

/// Is a blank line required between two consecutive entries? As between items; a statement
/// sits next to a constant or another statement, but not next to a function or a type.
pub(super) fn blank_before(prev: &Top, cur: &Top) -> bool {
    match (prev, cur) {
        (Top::Item(a), Top::Item(b)) => blank_between(a, b),
        (Top::Item(i), Top::Stmt(_)) | (Top::Stmt(_), Top::Item(i)) => !is_compact(i),
        (Top::Stmt(_), Top::Stmt(_)) => false,
    }
}
