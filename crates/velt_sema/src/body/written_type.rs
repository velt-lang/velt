//! A written type in a body ([`FnCx::resolve`]): its `typeof x` queries may name the locals in
//! scope, and a variable's may be `weak T | null` (`crate::weak_types`).

use velt_syntax::ast;

use super::FnCx;
use crate::hir;
use crate::weak_types::WeakPlace;

impl FnCx<'_, '_> {
    pub fn resolve(&mut self, t: &ast::TypeExpr) -> hir::TyId {
        self.resolve_as(t, false).0
    }

    /// [`resolve`](Self::resolve) the written type `t`, of a variable when `var` (where
    /// `weak T | null` may go, `crate::weak_types`); the flag says it is a `weak` type lowering
    /// handles (`hir::LocalDef::weak`).
    pub(super) fn resolve_as(&mut self, t: &ast::TypeExpr, var: bool) -> (hir::TyId, bool) {
        let env = self.env.clone();
        // `typeof x` may name the locals in scope (`crate::type_queries`).
        let mut heads = vec![];
        velt_syntax::visit::type_queries(t, &mut |path| {
            if let Some(h) = path.first() {
                heads.push(h.name.clone());
            }
        });
        let base = self.cx.query_locals.len();
        for h in heads {
            if let Some(ty) = self.peek_local_ty(&h) {
                self.cx.query_locals.push((h, ty));
            }
        }
        let ty = match var {
            true => {
                self.cx
                    .resolve_weak_annotation(t, &env, WeakPlace::Local, &mut |cx, t, env| {
                        cx.resolve_type(t, env)
                    })
            }
            false => (self.cx.resolve_type(t, &env), false),
        };
        self.cx.query_locals.truncate(base);
        ty
    }
}
