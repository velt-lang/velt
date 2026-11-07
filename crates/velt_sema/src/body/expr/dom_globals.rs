//! What server code does with `lib.dom`'s values, as on Node (#922, #2383): the types are std's
//! (std/lang/dom, generated), and the values Node doesn't have read as its `ReferenceError`
//! (`env_globals`). Here, the forms around such a read:
//!
//! - `x instanceof HTMLElement` evaluates `x`, then throws as reading `HTMLElement` does;
//!   `x instanceof CustomEvent` is `false`, since Node has the class but no Velt value is one.
//!   Where the test holds, `x` has the tested type, as under `tsc`: that code never runs, and a
//!   read of `x` there is the throw.
//! - `document.title = v` throws before `v` is evaluated, as reading `document` does; `v` is
//!   still checked; so is `k in g.p`.
//! - The arguments of a call that never happens (`document.addEventListener("click", (e) => …)`,
//!   whose receiver throws) are checked, and a closure's parameters without a type are `any`.

use velt_common::Span;
use velt_syntax::ast;

use super::dom_values::DOM_VALUES;
use super::env_globals::NODE_GLOBALS;
use crate::body::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};

/// What `x instanceof C` is for a `lib.dom` class `C` the program doesn't declare.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DomTest {
    /// Node has no such value: reading `C` throws.
    Throws,
    /// Node has the class (`Event`, `CustomEvent`), but no Velt value is an instance of it.
    False,
}

impl FnCx<'_, '_> {
    /// `C` of `x instanceof C`, when it is a `lib.dom` class the program doesn't declare.
    pub(crate) fn dom_test(&mut self, ty: &ast::TypeExpr) -> Option<DomTest> {
        let ast::TypeExprKind::Named { path, args } = &ty.kind else {
            return None;
        };
        let ([p], true) = (path.as_slice(), args.is_empty()) else {
            return None;
        };
        if self.browser_global(&p.name, p.span) {
            return Some(DomTest::Throws);
        }
        let node_class = NODE_GLOBALS
            .iter()
            .any(|(n, t)| *n == p.name && *t == "function");
        if !node_class || DOM_VALUES.binary_search(&p.name.as_str()).is_err() {
            return None;
        }
        let item = self.lookup_item(&p.name, p.span)?;
        self.prelude_type_only(&p.name, item)
            .then_some(DomTest::False)
    }

    /// `e instanceof C` for such a class (`None`: `C` is not one).
    pub(crate) fn dom_instance_test(
        &mut self,
        e: &ast::Expr,
        ty: &ast::TypeExpr,
        span: Span,
    ) -> Option<hir::Expr> {
        let test = self.dom_test(ty)?;
        let s = self.expr(e, None, Want::Borrow);
        let value = match test {
            DomTest::False => self.mk(
                hir::ExprKind::Lit(hir::Lit::Bool(false)),
                self.cx.ty.bool_,
                span,
            ),
            DomTest::Throws => {
                let ast::TypeExprKind::Named { path, .. } = &ty.kind else {
                    unreachable!("ICE: dom_test takes a named type");
                };
                self.not_defined(&path[0].name, path[0].span)
            }
        };
        let ty = value.ty;
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(s),
                span,
            }],
            value: Some(Box::new(value)),
            span,
        };
        Some(self.mk(hir::ExprKind::Block(block), ty, span))
    }

    /// The interface type `x` has where `x instanceof C` holds, for such a class.
    pub(crate) fn dom_narrow_target(&mut self, ty: &ast::TypeExpr) -> Option<TyId> {
        self.dom_test(ty)?;
        let t = self.resolve(ty);
        self.dom_type(t).then_some(t)
    }

    /// Is `t` one of std's `lib.dom` interfaces that has a value in `lib.dom`?
    pub(crate) fn dom_type(&self, t: TyId) -> bool {
        let TyKind::Dyn(d, _) = self.cx.ty.kind(t) else {
            return false;
        };
        self.cx.iface(*d).is_some_and(|i| {
            self.cx.scopes[i.module].is_std && DOM_VALUES.binary_search(&i.name.as_str()).is_ok()
        })
    }

    /// A read, where `instanceof` narrowed it to `target`, of a variable narrowed by such a
    /// test: the code never runs (the test threw or was `false`), so the read throws too.
    pub(crate) fn dom_narrowed_read(&mut self, target: TyId, span: Span) -> Option<hir::Expr> {
        if !self.dom_type(target) {
            return None;
        }
        let TyKind::Dyn(d, _) = self.cx.ty.kind(target) else {
            return None;
        };
        let name = self.cx.iface(*d)?.name.clone();
        let mut e = self.not_defined(&name, span);
        e.ty = target;
        Some(e)
    }

    /// The global another environment has that `e` reads first (`g` of `g.a[k].b`), if any.
    fn browser_root<'e>(&mut self, e: &'e ast::Expr) -> Option<&'e ast::Ident> {
        let mut root = e;
        loop {
            match &root.kind {
                ast::ExprKind::Paren(inner) => root = inner,
                ast::ExprKind::Member { object, .. } | ast::ExprKind::Index { object, .. } => {
                    root = object;
                }
                ast::ExprKind::Ident(id) if self.browser_global(&id.name, id.span) => {
                    return Some(id);
                }
                _ => return None,
            }
        }
    }

    /// `g.p = v`, `g.a.b op= v`, `g[k] = v` where `g` is another environment's global: reading
    /// `g` throws first, so nothing else runs; `v` is checked all the same.
    pub(crate) fn browser_global_write(
        &mut self,
        target: &ast::Expr,
        value: &ast::Expr,
    ) -> Option<hir::Expr> {
        if matches!(target.kind, ast::ExprKind::Ident(_)) {
            return None;
        }
        let id = self.browser_root(target)?;
        let h = self.browser_global_read(id)?;
        self.check_args_loose(std::slice::from_ref(value));
        Some(h)
    }

    /// `k in g.p` where `g` is another environment's global: `k`, then the throw.
    pub(crate) fn browser_global_in(
        &mut self,
        key: &ast::Expr,
        object: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        let id = self.browser_root(object)?;
        let k = self.expr(key, None, Want::Borrow);
        let h = self.browser_global_read(id)?;
        let ty = h.ty;
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(k),
                span,
            }],
            value: Some(Box::new(h)),
            span,
        };
        Some(self.mk(hir::ExprKind::Block(block), ty, span))
    }

    /// The arguments of a call whose receiver never gives a value (a browser global): checked,
    /// a closure's parameters without a type as `any`, since nothing calls it.
    pub(crate) fn check_args_unreached(&mut self, args: &[ast::Expr]) {
        for a in args {
            let expected = match &a.kind {
                ast::ExprKind::Arrow {
                    params,
                    type_params,
                    ..
                } if type_params.is_empty() && params.iter().any(|p| p.ty.is_none()) => {
                    self.any_ty().map(|any| {
                        let unit = self.cx.ty.unit;
                        self.cx.ty.fn_ptr(vec![any; params.len()], unit)
                    })
                }
                _ => None,
            };
            self.expr(a, expected, Want::Borrow);
        }
    }
}
