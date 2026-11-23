//! Module-level `let`s declared `weak` (`hir::GlobalDef::weak`, docs/internals/design/cycles.md
//! "`weak T`"): `let current: weak View | null = null` that functions use. The module state's slot
//! holds the target's slot, as a `weak` local's storage does (body/weak_locals.rs): every read is
//! `Intrinsic::WeakRead` of the bare `Global(d)`, the initializer and the values assigned are
//! borrowed, and a test does not narrow it. Module state is shared by the tasks that reach it one
//! at a time, under the module lock (semantics.md "Threads"), so its slot stays on one task at a
//! time as the weak core requires. A module `const` declared `weak` is refused: one evaluated at
//! each use would hold nothing.

use velt_common::Span;
use velt_syntax::ast;

use super::{FnCx, Want};
use crate::hir::{self, DefId, TyId};
use crate::resolve::TyEnv;
use crate::weak_types::WeakPlace;

/// How the initializer of a module variable is checked: a `weak` one's value stays its owner's,
/// borrowed as a weak field's is.
pub(super) fn init_want(weak: bool) -> Want {
    match weak {
        true => Want::Borrow,
        false => Want::Move,
    }
}

impl FnCx<'_, '_> {
    /// The written type `ann` of a global of `module` (`mutable`: a `let`, `var` or static field;
    /// `static`: a type's static field), and whether it is a `weak` module `let` lowering handles.
    /// A static field declared `weak` is not lowered yet: its paths go through the type.
    pub(super) fn global_annotation(
        &mut self,
        ann: Option<&ast::TypeExpr>,
        module: usize,
        (mutable, is_static): (bool, bool),
    ) -> (Option<TyId>, bool) {
        let place = match (is_static, mutable) {
            (true, _) => WeakPlace::Other,
            (false, true) => WeakPlace::ModuleLet,
            (false, false) => WeakPlace::ModuleConst,
        };
        let Some(t) = ann else {
            return (None, false);
        };
        let env = TyEnv::new(module, &[]);
        let (ty, weak) = (self.cx)
            .resolve_weak_annotation(t, &env, place, &mut |cx, t, env| cx.resolve_type(t, env));
        (Some(ty), weak && place == WeakPlace::ModuleLet)
    }

    /// The value of a `weak` module `let` written without one (`ty`, its read type): `null`.
    pub(super) fn weak_global_null(&mut self, ty: TyId, span: Span) -> hir::Expr {
        let n = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Lit(ast::Lit::Null),
            span,
        };
        self.expr_coerce(&n, ty, Want::Borrow)
    }

    /// The name of the `weak` module `let` whose narrowing token (`body::global_narrow`) is `l`:
    /// a test does not narrow it.
    pub(super) fn weak_global_token(&self, l: hir::LocalId) -> Option<String> {
        let t = self.f.global_tokens.iter().find(|t| t.token == l)?;
        let g = self.cx.global(t.def)?;
        g.weak.then(|| g.name.clone())
    }

    /// A read of the `weak` module `let` `d` (of read type `ty`): `Intrinsic::WeakRead`.
    pub(crate) fn weak_global_read(&mut self, d: DefId, ty: TyId, span: Span) -> hir::Expr {
        // An error at a read a test would have narrowed gets the note (`refuse_fact`).
        let token = self
            .f
            .global_tokens
            .iter()
            .find(|t| t.def == d)
            .map(|t| t.token);
        if let Some(t) = token {
            self.note_refused_read(t, span);
        }
        let place = self.mk(hir::ExprKind::Global(d), ty, span);
        self.intrinsic(hir::Intrinsic::WeakRead, vec![place], ty, span)
    }
}
