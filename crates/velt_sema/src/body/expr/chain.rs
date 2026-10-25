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
        let s = self.test_operand(object, None, Want::Borrow);
        let s = self.flatten_chain(s);
        if self.cx.ty.is_bottom(s.ty) {
            // The rest is still checked (its own errors), on the link's object bound as an
            // error value: the synthetic name must not be reported as unknown.
            let l = self.new_local(&name, s.ty, false, s.span, crate::body::LocalKind::Bind);
            self.push_scope();
            if let Some(scope) = self.f.scopes.last_mut() {
                scope.names.insert(name, l);
            }
            self.expr(&rest, None, want);
            self.pop_scope();
            return Some(self.error_expr(span));
        }
        let payload = self.present_payload(s.ty);
        // `any`, and `object` / `Function`, which hold `null` / `undefined` too (issue #926).
        let dynamic = self
            .cx
            .dynamic(s.ty)
            .is_some_and(|d| d != crate::any::Dynamic::Unknown);
        let (l, mode) = self.option_binding(&s, payload.unwrap_or(s.ty), &name, false);
        self.note_readonly_binding(l, object, &s);
        self.push_scope();
        if let Some(scope) = self.f.scopes.last_mut() {
            scope.names.insert(name, l);
        }
        let known = payload.is_some() || dynamic;
        let r = self.expr(&rest, if known { None } else { exp }, want);
        self.pop_scope();
        if dynamic {
            // `a?.b.c` on an `any` `a`: `undefined` when `a` is `null` or `undefined`.
            return Some(self.dynamic_chain_match(s, l, mode, r, span));
        }
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

    /// An optional chain whose rest gives a `T | null`, so that the chain is a
    /// `T | null | undefined` (`item.get("tags")?.at(0)`), as a `T | null` where the
    /// difference doesn't matter: the object of another optional link
    /// (`item.get("tags")?.at(0)?.asString()`) and the left side of `??` treat both nothings
    /// alike. The chain's short-circuit then gives `null` and its rest is not wrapped, which
    /// saves building and testing the three-state value. Anything else is returned as it is.
    pub(super) fn flatten_chain(&mut self, s: hir::Expr) -> hir::Expr {
        match self.cx.ty.opt_payload(s.ty) {
            Some(mid) if self.is_three_state(s.ty) && flattens(&s, mid) => self.flatten_to(s, mid),
            _ => s,
        }
    }

    /// [`Self::flatten_chain`] of `s`, which [`flattens`] to `mid`.
    fn flatten_to(&mut self, s: hir::Expr, mid: TyId) -> hir::Expr {
        let span = s.span;
        match s.kind {
            // The chain after the statements that evaluate its receiver's temporaries.
            H::Block(mut b) => {
                if let Some(value) = b.value.take() {
                    b.value = Some(Box::new(self.flatten_to(*value, mid)));
                }
                self.mk(H::Block(b), mid, span)
            }
            H::Match {
                scrutinee,
                mut arms,
            } => {
                let (Some(mut none_arm), Some(mut some_arm)) = (arms.pop(), arms.pop()) else {
                    unreachable!("ICE: a chain's match has two arms");
                };
                some_arm.body = match some_arm.body.kind {
                    H::WrapSome(inner) => *inner,
                    // A chain continued past another link (`a?.b()?.c()` checked as one chain):
                    // its rest is a chain of its own.
                    _ => self.flatten_to(some_arm.body, mid),
                };
                none_arm.body = self.mk(H::Lit(hir::Lit::Null), mid, span);
                let kind = H::Match {
                    scrutinee,
                    arms: vec![some_arm, none_arm],
                };
                self.mk(kind, mid, span)
            }
            _ => unreachable!("ICE: `flattens` accepted only blocks and matches"),
        }
    }
}

/// Is `e` an optional chain's short-circuit whose rest gives `mid` (a `T | null`) wrapped as a
/// `T | null | undefined` ([`FnCx::flatten_chain`]): `match (o) { l => rest, _ => undefined }`,
/// where `rest` is `WrapSome` of a `mid` value or such a chain itself, possibly after a block's
/// statements.
fn flattens(e: &hir::Expr, mid: TyId) -> bool {
    match &e.kind {
        H::Block(b) => b.value.as_ref().is_some_and(|v| flattens(v, mid)),
        H::Match { arms, .. } => {
            arms.len() == 2
                && arms.iter().all(|a| a.guard.is_none())
                && matches!(arms[1].body.kind, H::Lit(hir::Lit::Null))
                && match &arms[0].body.kind {
                    H::WrapSome(inner) => inner.ty == mid,
                    _ => arms[0].body.ty == e.ty && flattens(&arms[0].body, mid),
                }
        }
        _ => false,
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
