//! `for...of` over a `Map` (or its `keys()`, `values()`, `entries()`) held in a place iterates
//! live, as in JS: entries added by the body are visited, deleted ones are not (#624). The loop
//! is desugared into a cursor walk over the map's entries (std/prelude/map.vlt, "Live
//! iteration"):
//!
//! ```text
//! {
//!   const <map@N> = m;                          // the map object, as JS holds it
//!   let <cursor@N> = <map@N>.__cursor();        // a struct: no allocation
//!   label: while (<map@N>.__advance(<cursor@N>)) {
//!     kind pattern = <map@N>.__entryAt(<cursor@N>);   // __keyAt, __valueAt
//!     { body }
//!   }
//! }
//! ```
//!
//! `m` is evaluated once, at loop entry, into a hidden local referring to the same map: a body
//! that assigns another map to `m` goes on iterating the original one, as in JS. For a field
//! (`o.inner.m`) the hidden local holds a share of the map, which keeps it alive when the body
//! replaces the field through another reference to its object. A `const`
//! variable or `this` cannot be assigned, so there `<map@N>` is `m` itself: a second name for
//! a map that is still used can make the program count its maps. Only a place
//! written without calls qualifies (a variable, `this`, fields of those; no getter); the body
//! may change the map, since nothing borrows it between steps. Any other source iterates the
//! array `entries()`, `keys()` or `values()` returns (`loops.rs`).

use velt_syntax::ast;

use super::for_iter::ForOfParts;
use super::FnCx;
use crate::hir::{self, ExprKind as H};

impl FnCx<'_, '_> {
    /// Lower `for (p of iter)` as a live map loop when `iter` (checked: `it`) is a map place or
    /// a `keys()`, `values()` or `entries()` call on one. False (nothing done) otherwise.
    pub(super) fn for_of_map(
        &mut self,
        iter: &ast::Expr,
        it: &hir::Expr,
        p: ForOfParts<'_>,
        out: &mut Vec<hir::Stmt>,
    ) -> bool {
        let Some((recv, read)) = self.live_map_source(iter, it) else {
            return false;
        };
        let mut names = CursorLoop::new(p.span, recv, iter.span);
        names.held = !self.fixed_source(it);
        let block = ast::Stmt {
            kind: ast::StmtKind::Block(names.desugar(&p, read)),
            span: p.span,
        };
        self.stmt(&block, out);
        if names.held {
            if let Some(hir::Stmt {
                kind: hir::StmtKind::Block(b),
                ..
            }) = out.last_mut()
            {
                if let Some(first) = b.stmts.first_mut() {
                    self.own_hold(first);
                }
            }
        }
        true
    }

    /// The hidden local of a loop over a field (`o.inner.m`) is bound by reference to it
    /// (`const_borrow`), but the body may replace the field through another name for its object
    /// (`inn.m = …`), which frees the map the loop walks: hold a share of the map instead.
    fn own_hold(&mut self, s: &mut hir::Stmt) {
        let hir::StmtKind::LetPat { pat, init } = &mut s.kind else {
            return;
        };
        let hir::PatKind::Binding(local, hir::UseMode::Borrow) = pat.kind else {
            return;
        };
        let (ty, span) = (init.ty, init.span);
        let place = std::mem::replace(init, self.error_expr(span));
        let kind = H::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::Share),
            args: vec![place],
        };
        let init = self.mk(kind, ty, span);
        s.kind = hir::StmtKind::Let {
            local,
            init: Some(init),
        };
    }

    /// Does the map source `it` (checked), or the receiver of its `keys()`, `values()` or
    /// `entries()` call, always name the same map: a `const` variable or `this`?
    fn fixed_source(&self, it: &hir::Expr) -> bool {
        let src = match &it.kind {
            H::Call { args, .. } => match args.first() {
                Some(r) => r,
                None => return false,
            },
            _ => it,
        };
        match &src.kind {
            H::Local(id, _) => matches!(
                self.local_kind(*id),
                super::LocalKind::Const | super::LocalKind::Using | super::LocalKind::This
            ),
            _ => false,
        }
    }

    /// The map place and the cursor method reading each value, when `iter` is one per the
    /// module docs.
    fn live_map_source<'e>(
        &mut self,
        iter: &'e ast::Expr,
        it: &hir::Expr,
    ) -> Option<(&'e ast::Expr, &'static str)> {
        if super::places::is_place(it) && self.is_prelude_map(it.ty) {
            return pure_place(iter).then_some((iter, "__entryAt"));
        }
        let ast::ExprKind::Call { callee, args, .. } = &iter.kind else {
            return None;
        };
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let read = match prop.name.as_str() {
            "keys" => "__keyAt",
            "values" => "__valueAt",
            "entries" => "__entryAt",
            _ => return None,
        };
        let H::Call { args: hargs, .. } = &it.kind else {
            return None;
        };
        let recv = hargs.first()?;
        let live = args.is_empty()
            && pure_place(object)
            && super::places::is_place(recv)
            && self.is_prelude_map(recv.ty);
        live.then_some((&**object, read))
    }
}

/// Is `e` a variable, `this` or a field of one (no call, index or optional chain), so
/// evaluating it again at every step reads the same map?
fn pure_place(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => pure_place(object),
        ast::ExprKind::Paren(x) => pure_place(x),
        _ => false,
    }
}

/// The synthesized pieces of one live map loop.
struct CursorLoop<'e> {
    cursor: String,
    /// The hidden local holding the map for the whole loop.
    hold: String,
    /// Whether the loop uses `hold` (otherwise the map expression itself).
    held: bool,
    map: &'e ast::Expr,
    /// The iterated expression: where the value reads point (a diagnostic about them is one
    /// about iterating the map).
    iter: velt_common::Span,
    span: velt_common::Span,
    /// Empty, at the `for` keyword (as `for_iter.rs`'s hidden locals).
    at: velt_common::Span,
}

impl<'e> CursorLoop<'e> {
    fn new(span: velt_common::Span, map: &'e ast::Expr, iter: velt_common::Span) -> Self {
        CursorLoop {
            cursor: format!("<cursor@{}>", span.lo),
            hold: format!("<map@{}>", span.lo),
            held: true,
            map,
            iter,
            span,
            at: velt_common::Span::new(span.file, span.lo, span.lo),
        }
    }

    /// The block of the module docs; `read` is the cursor method giving each value.
    fn desugar(&self, p: &ForOfParts<'_>, read: &str) -> ast::Block {
        let cursor = self.var(
            ast::VarKind::Let,
            self.ident_pat(&self.cursor),
            self.on_map("__cursor", vec![]),
        );
        let value = ast::Expr {
            span: self.iter,
            ..self.on_map(read, vec![self.name()])
        };
        let bind = self.var(p.kind, p.pattern.clone(), value);
        let body = ast::Stmt {
            kind: ast::StmtKind::Block(p.body.clone()),
            span: p.body.span,
        };
        let mut lp = self.stmt(ast::StmtKind::While {
            cond: self.on_map("__advance", vec![self.name()]),
            body: self.block(vec![bind, body]),
        });
        if let Some(l) = p.label {
            lp = self.stmt(ast::StmtKind::Labeled {
                label: l.clone(),
                body: Box::new(lp),
            });
        }
        let mut stmts = vec![cursor, lp];
        if self.held {
            let hold = self.ident_pat(&self.hold);
            stmts.insert(0, self.var(ast::VarKind::Const, hold, self.map.clone()));
        }
        self.block(stmts)
    }

    /// `<map@N>.method(args)`.
    fn on_map(&self, method: &str, args: Vec<ast::Expr>) -> ast::Expr {
        let map = if self.held {
            self.expr(ast::ExprKind::Ident(self.ident(&self.hold)))
        } else {
            self.map.clone()
        };
        let callee = self.expr(ast::ExprKind::Member {
            object: Box::new(map),
            prop: self.ident(method),
            optional: false,
        });
        self.expr(ast::ExprKind::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args,
            optional: false,
        })
    }

    fn name(&self) -> ast::Expr {
        self.expr(ast::ExprKind::Ident(self.ident(&self.cursor)))
    }

    fn var(&self, kind: ast::VarKind, pattern: ast::Pattern, init: ast::Expr) -> ast::Stmt {
        self.stmt(ast::StmtKind::Var(ast::VarDecl {
            kind,
            pattern,
            ty: None,
            init: Some(init),
            span: self.at,
        }))
    }

    fn expr(&self, kind: ast::ExprKind) -> ast::Expr {
        ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span: self.at,
        }
    }

    fn stmt(&self, kind: ast::StmtKind) -> ast::Stmt {
        ast::Stmt {
            kind,
            span: self.at,
        }
    }

    fn block(&self, stmts: Vec<ast::Stmt>) -> ast::Block {
        ast::Block {
            stmts,
            span: self.span,
        }
    }

    fn ident(&self, name: &str) -> ast::Ident {
        ast::Ident {
            name: name.to_string(),
            span: self.at,
        }
    }

    fn ident_pat(&self, name: &str) -> ast::Pattern {
        ast::Pattern {
            id: ast::NodeId(u32::MAX),
            kind: ast::PatternKind::Ident(self.ident(name)),
            span: self.at,
        }
    }
}
