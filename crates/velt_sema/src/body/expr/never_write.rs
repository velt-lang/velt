//! `x.f = v`, `x[k] = v`, `x.f += v`, `x.f++` where `x` is `never`: a read that throws, such as a
//! browser global on Node (`document.createElement("div").textContent = s`, `env_globals.rs`).
//! The write never runs, as in JavaScript, where reading `x` throws first. A member of `never` is
//! `never` too, which no value converts to, so the write is checked here: `x`, then the key and
//! the value on their own types, with `x` diverging first.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, StmtKind as S};

impl FnCx<'_, '_> {
    /// The write to a member of a `never` value `target` names (`None`: `target` is another
    /// place), its `value` (`None` for `x.f++`) checked on its own.
    pub(super) fn never_receiver_write(
        &mut self,
        target: &ast::Expr,
        value: Option<&ast::Expr>,
        span: Span,
    ) -> Option<hir::Expr> {
        // An optional chain (`x?.f = v`) is no assignment target, `never` or not.
        let (object, key) = match &target.kind {
            ast::ExprKind::Paren(inner) => return self.never_receiver_write(inner, value, span),
            ast::ExprKind::Member {
                object,
                optional: false,
                ..
            } => (object, None),
            ast::ExprKind::Index {
                object,
                index,
                optional: false,
                ..
            } => (object, Some(index)),
            _ => return None,
        };
        // Checked twice only where it is a browser global read (`may_be_never`): other `never`
        // values keep the plain write's error.
        if !self.may_be_never(object) || self.trial_type(object)? != self.cx.ty.never {
            return None;
        }
        let parts = [Some(&**object), key.map(|k| &**k)];
        let stmts: Vec<hir::Stmt> = (parts.into_iter().flatten())
            .map(|e| {
                let h = self.expr(e, None, Want::Borrow);
                hir::Stmt {
                    kind: S::Expr(h),
                    span: e.span,
                }
            })
            .collect();
        // The value never runs either: checked as a browser call's arguments are, a closure's
        // untyped parameters as `any`.
        if let Some(v) = value {
            self.check_args_unreached(std::slice::from_ref(v));
        }
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        let never = self.cx.ty.never;
        Some(self.mk(hir::ExprKind::Block(block), never, span))
    }

    /// May `object` be `never` as a browser global read on the server is: is it rooted (through
    /// members, indexes, calls, `!` and casts) at a browser global, or at a `const` whose value is
    /// (`browser_locals`)? Not another `never` value (a parameter, a field of an interface read
    /// through a cast `any`): one may be a value at run time, which a write must not skip.
    fn may_be_never(&mut self, object: &ast::Expr) -> bool {
        let mut root = object;
        loop {
            root = match &root.kind {
                ast::ExprKind::Member { object, .. } | ast::ExprKind::Index { object, .. } => {
                    object
                }
                ast::ExprKind::Call { callee, .. } => callee,
                ast::ExprKind::Paren(inner) | ast::ExprKind::NonNull(inner) => inner,
                ast::ExprKind::Cast { expr, .. } => expr,
                _ => break,
            };
        }
        let ast::ExprKind::Ident(id) = &root.kind else {
            return false;
        };
        if self.is_local_name(&id.name) {
            return self.peek_browser_local(&id.name);
        }
        self.browser_global(&id.name, id.span)
    }

    /// Is the visible local `name` a browser local, here or in the enclosing function a closure
    /// captures it from (a capture is that `const`, which never changes)?
    fn peek_browser_local(&self, name: &str) -> bool {
        let frames = std::iter::once(&self.f).chain(self.outer.iter().rev());
        for f in frames {
            if let Some(l) = crate::body::locals::frame_lookup(f, name) {
                if f.kinds[l.0 as usize] != crate::body::LocalKind::Capture {
                    return f.browser_locals.contains(&l);
                }
            }
        }
        false
    }

    /// `const name = init`, declared: a browser local when `init` is rooted at a browser global
    /// (`may_be_never`) and is `never` here. Only a `const`: a `let` may later hold another
    /// `never` value, also from a closure that runs before a write written above it.
    pub(in crate::body) fn note_browser_local(&mut self, v: &ast::VarDecl, name: &ast::Ident) {
        let Some(init) = v.init.as_ref().filter(|_| v.kind == ast::VarKind::Const) else {
            return;
        };
        let Some(l) = self.declared_local(&name.name) else {
            return;
        };
        if self.local_ty(l) == self.cx.ty.never && self.may_be_never(init) {
            self.f.browser_locals.insert(l);
        }
    }
}
