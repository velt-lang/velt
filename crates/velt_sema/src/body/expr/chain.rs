//! Optional chains short-circuit the rest of the chain, as in TS: in `u?.name.length` a null
//! `u` makes the whole chain `null` and `.length` is never evaluated. A member access, index or
//! call that continues a chain past a `?.` link is checked as `link?.<rest>`: the link's object
//! is evaluated once, and the rest of the chain is checked on its (non-null) value, bound to a
//! synthetic local the rewritten rest names. Parentheses end a chain (`(u?.name).length`).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, PatKind as P, TyId};

impl FnCx<'_, '_> {
    /// `e` when it continues an optional chain past a `?.` link (`a?.b.c`, `a?.b.f()`).
    pub(super) fn short_circuit(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
        want: Want,
    ) -> Option<hir::Expr> {
        if is_optional(e) {
            return None;
        }
        let link = nearest_link(chain_object(e)?)?;
        let object = chain_object(link)?;
        let name = format!("<chain@{}>", link.span.lo);
        let rest = rewrite(e, link.span, &name, object.span);
        let span = e.span;
        let s = self.expr(object, None, Want::Borrow);
        if self.cx.ty.is_bottom(s.ty) {
            self.expr(&rest, None, want);
            return Some(self.error_expr(span));
        }
        let payload = self.cx.ty.opt_payload(s.ty);
        let (l, mode) = self.option_binding(&s, payload.unwrap_or(s.ty), &name, false);
        if self.is_record_read(&s) {
            self.f.record_copies.insert(l);
        }
        self.push_scope();
        if let Some(scope) = self.f.scopes.last_mut() {
            scope.names.insert(name, l);
        }
        let r = self.expr(&rest, if payload.is_some() { None } else { exp }, want);
        self.pop_scope();
        Some(match payload {
            Some(p) => self.chain_match(s, l, mode, p, r, span),
            None => {
                // A non-null link object: nothing to short-circuit, only bind it.
                let ty = r.ty;
                let pat = self.pat(P::Binding(l, mode), s.ty, span);
                let arms = vec![hir::Arm {
                    pat,
                    guard: None,
                    body: r,
                }];
                let kind = H::Match {
                    scrutinee: Box::new(s),
                    arms,
                };
                self.mk(kind, ty, span)
            }
        })
    }
}

/// Is `e` itself an optional link (`a?.b`, `a?.[i]`, `f?.()`)?
fn is_optional(e: &ast::Expr) -> bool {
    matches!(
        e.kind,
        ast::ExprKind::Member { optional: true, .. }
            | ast::ExprKind::Index { optional: true, .. }
            | ast::ExprKind::Call { optional: true, .. }
    )
}

/// The object (or callee) a chain element applies to.
fn chain_object(e: &ast::Expr) -> Option<&ast::Expr> {
    match &e.kind {
        ast::ExprKind::Member { object, .. } | ast::ExprKind::Index { object, .. } => Some(object),
        ast::ExprKind::Call { callee, .. } => Some(callee),
        _ => None,
    }
}

/// The nearest optional link at or below `e` in its chain.
fn nearest_link(e: &ast::Expr) -> Option<&ast::Expr> {
    if is_optional(e) {
        return Some(e);
    }
    nearest_link(chain_object(e)?)
}

/// `e` with the link at `link` made non-optional and its object replaced by the local `name`.
fn rewrite(e: &ast::Expr, link: Span, name: &str, at: Span) -> ast::Expr {
    let mut out = e.clone();
    let mut cur = &mut out;
    loop {
        let is_link = cur.span == link && is_optional(cur);
        let (object, optional) = match &mut cur.kind {
            ast::ExprKind::Member {
                object, optional, ..
            }
            | ast::ExprKind::Index {
                object, optional, ..
            } => (object, optional),
            ast::ExprKind::Call {
                callee, optional, ..
            } => (callee, optional),
            _ => unreachable!("ICE: optional chain link not found"),
        };
        if is_link {
            *optional = false;
            let at = Span {
                lo: at.hi,
                hi: at.hi,
                ..at
            };
            **object = ast::Expr {
                id: ast::NodeId(u32::MAX),
                kind: ast::ExprKind::Ident(ast::Ident {
                    name: name.to_string(),
                    span: at,
                }),
                span: at,
            };
            return out;
        }
        cur = object;
    }
}
