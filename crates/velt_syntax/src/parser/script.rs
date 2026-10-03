//! Top-level statements (a script, as in a TS or JS file): when the root file has statements
//! outside declarations, they run, in order, in a generated `main` (`async` when one of them
//! awaits). A top-level `const`/`let` moves into `main` with them, unless a declaration (a
//! function, class, struct, `extend` block or a module constant that stays) refers to it
//! (`script_names`): then it stays a module constant, which those declarations can see.
//!
//! The generated function's name has an empty span at the first statement; sema uses that to
//! reject statements in an imported module (only the root file runs them).

use super::Parser;
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::Span;

impl Parser<'_> {
    /// Does the cursor start a declaration (as opposed to a statement)?
    pub(super) fn at_item_start(&mut self) -> bool {
        match self.cur_kw() {
            Some(
                Kw::Export
                | Kw::Import
                | Kw::Function
                | Kw::Struct
                | Kw::Class
                | Kw::Interface
                | Kw::Enum
                | Kw::Const
                | Kw::Let
                | Kw::Declare,
            ) => true,
            // `import(...)`/`import.meta` are not supported, so `import` always starts an item.
            Some(Kw::Async) => self.nth(1) == Tok::Kw(Kw::Function),
            Some(Kw::Type) => Self::is_ident_like(self.nth(1)),
            None => self.at_extend(),
            _ => false,
        }
    }

    /// Turn the root file's top-level `stmts` into a generated `main` (see the module docs).
    pub(super) fn finish_script(&mut self, items: &mut Vec<Item>, stmts: Vec<Stmt>) {
        let first = stmts[0].span;
        let has_main = items
            .iter()
            .any(|i| matches!(&i.kind, ItemKind::Function(f) if f.sig.name.name == "main"));
        if has_main {
            self.error(
                "top-level statements cannot be combined with `function main()`; move them into `main`",
                first,
            );
            return;
        }
        let keep = self.kept_vars(items);
        let mut body = stmts;
        let mut rest = vec![];
        for (item, keep) in std::mem::take(items).into_iter().zip(keep) {
            match item.kind {
                ItemKind::Var(v) if !keep => body.push(Stmt {
                    kind: StmtKind::Var(v),
                    span: item.span,
                }),
                kind => rest.push(Item { kind, ..item }),
            }
        }
        body.sort_by_key(|s| s.span.lo);
        let is_async = body.iter().any(|s| self.mentions(s.span, "await"));
        let last = body.iter().map(|s| s.span.hi).max().unwrap_or(first.hi);
        let span = Span::new(self.file, first.lo, last);
        let sig = FnSig {
            name: Ident {
                name: "main".into(),
                span: Span::new(self.file, first.lo, first.lo),
            },
            generics: vec![],
            params: vec![],
            ret: None,
            throws: None,
            is_async,
            is_generator: false,
            span,
        };
        rest.push(Item {
            kind: ItemKind::Function(FnDecl {
                sig,
                body: Block { stmts: body, span },
            }),
            exported: false,
            span,
        });
        *items = rest;
    }

    /// Per item: is it a top-level variable that stays at module level? Exported ones do, and
    /// those a staying declaration refers to (to a fixed point).
    fn kept_vars(&self, items: &[Item]) -> Vec<bool> {
        let mut keep: Vec<bool> = items
            .iter()
            .map(|i| !matches!(i.kind, ItemKind::Var(_)) || i.exported)
            .collect();
        let free: Vec<_> = items.iter().map(super::script_names::free_names).collect();
        loop {
            let mut changed = false;
            for (i, item) in items.iter().enumerate() {
                let ItemKind::Var(v) = &item.kind else {
                    continue;
                };
                if keep[i] {
                    continue;
                }
                let names = bound_names(&v.pattern);
                let used = (0..items.len())
                    .any(|j| j != i && keep[j] && names.iter().any(|n| free[j].contains(n)));
                if used {
                    keep[i] = true;
                    changed = true;
                }
            }
            if !changed {
                return keep;
            }
        }
    }

    /// Does the source text in `span` contain `word` as a whole identifier, not after a `.`
    /// (`x.word` is a member, not a reference)?
    fn mentions(&self, span: Span, word: &str) -> bool {
        let text = &self.src[span.lo as usize..span.hi as usize];
        let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
        text.match_indices(word).any(|(at, _)| {
            let before = text[..at].trim_end().chars().next_back();
            let touching = text[..at].chars().next_back().is_some_and(ident);
            let after = text[at + word.len()..].chars().next();
            !touching && before != Some('.') && !after.is_some_and(ident)
        })
    }
}

/// The names a declaration pattern binds.
fn bound_names(p: &Pattern) -> Vec<String> {
    match &p.kind {
        PatternKind::Ident(i) => vec![i.name.clone()],
        PatternKind::Wildcard => vec![],
        PatternKind::Object { fields, rest } => fields
            .iter()
            .flat_map(|(_, p)| bound_names(p))
            .chain(rest.iter().map(|r| r.name.clone()))
            .collect(),
        PatternKind::Array { elems, rest } => elems
            .iter()
            .flat_map(bound_names)
            .chain(rest.iter().map(|r| r.name.clone()))
            .collect(),
        PatternKind::Default { pattern, .. } => bound_names(pattern),
    }
}
