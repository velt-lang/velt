//! `for` loops. The parser desugars comma lists in a C-style head (velt_syntax
//! `parser/for_loop.rs`): several declarations become a block around the loop that starts at
//! the `for` keyword, and several updates become `(() => { u1; u2; })()` with empty spans on the
//! arrow and its parentheses. Both are printed back in the comma form the user wrote. A
//! `for (const k in o)` is a `for...of` over `Object.keys(o)` whose callee has an empty span,
//! printed back as `for...in`.

use velt_syntax::ast::{ArrowBody, Block, Expr, ExprKind, Pattern, Stmt, StmtKind, VarKind};

use super::Printer;
use crate::doc::{cat, group, indent, join, nil, softline, Doc};

impl<'a> Printer<'a> {
    /// `for (init; cond; update) body`, with an optional label in front.
    pub(super) fn for_stmt(
        &mut self,
        label: Option<&str>,
        init: &[Stmt],
        cond: Option<&Expr>,
        update: Option<&Expr>,
        body: &Block,
    ) -> Doc {
        let label = label.map_or_else(nil, |l| cat![l.to_string(), ": "]);
        if init.is_empty() && cond.is_none() && update.is_none() {
            return cat![label, "for (;;) ", self.block(body)];
        }
        let init = self.for_init(init);
        let cond = cond.map_or_else(nil, |c| cat![" ", self.expr(c)]);
        let update = update.map_or_else(nil, |u| cat![" ", self.for_update(u)]);
        let head = cat![init, ";", cond, ";", update];
        cat![
            label,
            group(cat![
                "for (",
                indent(cat![softline(), head]),
                softline(),
                ")"
            ]),
            " ",
            self.block(body)
        ]
    }

    pub(super) fn for_of(
        &mut self,
        kind: VarKind,
        pattern: &Pattern,
        iter: &Expr,
        body: &Block,
        is_await: bool,
    ) -> Doc {
        let (word, iter) = match for_in_object(iter) {
            Some(object) => (" in ", object),
            None => (" of ", iter),
        };
        let head = cat![
            kind.keyword(),
            " ",
            self.pattern(pattern),
            word,
            self.expr(iter)
        ];
        let open = if is_await { "for await (" } else { "for (" };
        cat![open, head, ") ", self.block(body)]
    }

    /// A block the parser made from a `for` with several declarations, printed as that `for`.
    pub(super) fn desugared_for(&mut self, block: &Block) -> Option<Doc> {
        if !self.src[block.span.lo as usize..].starts_with("for") {
            return None;
        }
        let (last, init) = block.stmts.split_last()?;
        let (label, lp) = match &last.kind {
            StmtKind::Labeled { label, body } => (Some(label.name.as_str()), body.as_ref()),
            _ => (None, last),
        };
        let StmtKind::For {
            init: None,
            cond,
            update,
            body,
        } = &lp.kind
        else {
            return None;
        };
        Some(self.for_stmt(label, init, cond.as_ref(), update.as_ref(), body))
    }

    /// Declarations share the first one's keyword (`let a = 1, b = 2`); expressions are listed.
    fn for_init(&mut self, init: &[Stmt]) -> Doc {
        let parts = init
            .iter()
            .enumerate()
            .map(|(i, s)| match &s.kind {
                StmtKind::Var(var) if i == 0 => self.var_decl(var),
                StmtKind::Var(var) => self.declarator(var),
                StmtKind::Expr(e) => self.expr(e),
                _ => self.stmt(s),
            })
            .collect();
        join(&", ".into(), parts)
    }

    fn for_update(&mut self, update: &Expr) -> Doc {
        match update_sequence(update) {
            Some(stmts) => {
                let parts = stmts
                    .iter()
                    .map(|s| match &s.kind {
                        StmtKind::Expr(e) => self.expr(e),
                        _ => self.stmt(s),
                    })
                    .collect();
                join(&", ".into(), parts)
            }
            None => self.expr(update),
        }
    }
}

/// The updates of a desugared `u1, u2, …` list: an argument-less call of a parenthesized
/// block-bodied arrow, both with empty spans (a source expression's span is never empty).
fn update_sequence(update: &Expr) -> Option<&[Stmt]> {
    let ExprKind::Call { callee, args, .. } = &update.kind else {
        return None;
    };
    let ExprKind::Paren(arrow) = &callee.kind else {
        return None;
    };
    let ExprKind::Arrow {
        body: ArrowBody::Block(b),
        ..
    } = &arrow.kind
    else {
        return None;
    };
    let synthesized =
        args.is_empty() && callee.span.lo == callee.span.hi && arrow.span.lo == arrow.span.hi;
    synthesized.then_some(b.stmts.as_slice())
}

/// The object of a desugared `for (const k in o)`: the loop's iterable is `Object.keys(o)` with
/// an empty-span callee (a source expression's span is never empty).
fn for_in_object(iter: &Expr) -> Option<&Expr> {
    let ExprKind::Call { callee, args, .. } = &iter.kind else {
        return None;
    };
    let [object] = args.as_slice() else {
        return None;
    };
    (callee.span.lo == callee.span.hi && matches!(callee.kind, ExprKind::Member { .. }))
        .then_some(object)
}
