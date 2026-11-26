//! `v as Promise<T>` of an `any` / `unknown` `v` (#924 step 4): TypeScript's `Promise<T>` may
//! reject with anything, and the promise `v` holds may reject with any value, so the cast's
//! type rejects with `unknown` (`Promise<T, unknown>`), as an `await` of an `any` does. A written
//! `Promise<T>` elsewhere infers what it rejects with from what is stored into it
//! (`throws::promise_sites`); a cast's type has nothing stored into it, and `never` there would
//! make the held promise's rejection impossible to pass on. A written error type
//! (`Promise<T, E>`) is kept: the reason is checked against it when the promise rejects.
//! Lowering converts the held promise (velt_vir `any_promise_cast.rs`).

use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

/// Is the written cast type `ty` a `Promise<T>` without an error type, alone or with
/// `| null` / `| undefined`?
pub(crate) fn rejects_anything(ty: &ast::TypeExpr) -> bool {
    let promise = |t: &ast::TypeExpr| match &t.kind {
        ast::TypeExprKind::Named { path, args } => {
            path.len() == 1 && path[0].name == "Promise" && args.len() == 1
        }
        _ => false,
    };
    let nothing = |t: &ast::TypeExpr| match &t.kind {
        ast::TypeExprKind::Named { path, args } => {
            args.is_empty()
                && path.len() == 1
                && matches!(path[0].name.as_str(), "null" | "undefined")
        }
        _ => false,
    };
    match &ty.kind {
        ast::TypeExprKind::Union(ms) => {
            ms.iter().filter(|m| promise(m)).count() == 1
                && ms.iter().all(|m| promise(m) || nothing(m))
        }
        _ => promise(ty),
    }
}

impl FnCx<'_, '_> {
    /// The cast type `target` (a promise, or an optional one) rejecting with `unknown` (module
    /// docs).
    pub(super) fn open_promise_error(&mut self, target: TyId) -> TyId {
        let Some(defs) = self.cx.any else {
            return target;
        };
        let unknown = self.cx.ty.intern(TyKind::Dyn(defs.unknown, vec![]));
        match self.cx.ty.kind(target).clone() {
            TyKind::Promise(t, _) => self.cx.ty.intern(TyKind::Promise(t, unknown)),
            TyKind::Option(p, n) => match self.cx.ty.kind(p).clone() {
                TyKind::Promise(t, _) => {
                    let p = self.cx.ty.intern(TyKind::Promise(t, unknown));
                    self.cx.ty.intern(TyKind::Option(p, n))
                }
                _ => target,
            },
            _ => target,
        }
    }
}
