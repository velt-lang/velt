//! Function values the prelude makes, called from user code: a `new Promise` executor's `resolve`
//! and `reject` (`promiseNew`) and the fields of a std object type declared as function types
//! (`Promise.withResolvers`' `resolve` and `reject`). Their types are std's, which are no sites
//! (`throws::fn_types`): a call of one throws what its type says, nothing more, not what the
//! user's function types of the same shape hold, which the structural match of a call through a
//! value of no known site would add (#3949). A user function value can't be stored into a std
//! function type unless it throws within that type (the conversion is refused), and a user type
//! with a site of the same shape is std's object type only while the site's set is empty.
//!
//! An executor parameter the executor assigns, or a closure in it does, is noted too: the value
//! assigned brings its error type into the parameter's type (`r = open` for a `(n: number) =>
//! void` variable holding a throwing closure: its site's variable), so a call of the parameter
//! throws that through its type, which the structural match would only repeat. A `const` without
//! a written type initialized with one holds the same function.

use velt_common::Span;
use velt_syntax::ast;

use crate::hir::{ExprKind as H, Stmt, StmtKind as S, TyKind};

impl crate::body::FnCx<'_, '_> {
    /// `executor` is a `new Promise` executor (an arrow): note its parameters without a written
    /// type.
    pub(crate) fn note_executor_params(&mut self, executor: &ast::Expr) {
        let mut arrow = executor;
        while let ast::ExprKind::Paren(x) = &arrow.kind {
            arrow = x;
        }
        let ast::ExprKind::Arrow { params, .. } = &arrow.kind else {
            return;
        };
        if self.cx.fn_sites.by_file.is_empty() {
            return;
        }
        for a in params {
            if a.ty.is_none() && a.pattern.is_none() {
                self.executor_params.insert(a.name.span);
            }
        }
    }

    /// `const k = r` without a written type, of such a parameter (the last statement of `out`
    /// declares it): `k` holds the prelude's function too.
    pub(crate) fn note_std_const(&mut self, v: &ast::VarDecl, name: &ast::Ident, out: &[Stmt]) {
        if v.kind != ast::VarKind::Const || v.ty.is_some() || self.executor_params.is_empty() {
            return;
        }
        let Some(S::Let {
            init: Some(init), ..
        }) = out.last().map(|s| &s.kind)
        else {
            return;
        };
        if self.std_fn_value(init) {
            self.executor_params.insert(name.span);
        }
    }

    /// Is `h` a function value the prelude made: a parameter
    /// [`note_executor_params`](Self::note_executor_params) noted or a `const` holding one (read
    /// where it is declared or in a closure there), or a std object type's field declared as a
    /// function type?
    pub(crate) fn std_fn_value(&self, h: &crate::hir::Expr) -> bool {
        match &h.kind {
            H::Local(l, _) => {
                !self.executor_params.is_empty()
                    && (self.defining_local(*l)).is_some_and(|d| self.executor_params.contains(&d))
            }
            H::Field { base, index, .. } => {
                let TyKind::Adt(d, _) = self.cx.ty.kind(base.ty) else {
                    return false;
                };
                let Some(a) = self.cx.adt(*d) else {
                    return false;
                };
                let std = self.cx.scopes.get(a.module).is_some_and(|m| m.is_std);
                let declared = (a.fields.get(*index as usize)).map(|f| f.declared);
                let declared = declared.map(|t| self.cx.ty.core(t).unwrap_or(t));
                std && declared.is_some_and(|t| matches!(self.cx.ty.kind(t), TyKind::FnPtr { .. }))
            }
            _ => false,
        }
    }

    /// The span of the local `l` is (through the captures of the enclosing closures).
    fn defining_local(&self, l: crate::hir::LocalId) -> Option<Span> {
        let mut frames = std::iter::once(&self.f).chain(self.outer.iter().rev());
        let mut f = frames.next()?;
        let mut l = l;
        for _ in 0..64 {
            let i = l.0 as usize;
            let def = f.locals.get(i)?;
            if f.kinds.get(i) != Some(&crate::body::LocalKind::Capture) {
                return Some(def.span);
            }
            l = f.captures.iter().find(|c| c.inner == l)?.outer;
            f = frames.next()?;
        }
        None
    }
}
