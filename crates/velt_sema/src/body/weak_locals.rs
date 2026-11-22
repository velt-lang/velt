//! `weak` local variables (`hir::LocalDef::weak`, docs/internals/design/cycles.md "`weak T`"):
//! `let parent: weak Node | null = node.parent` in a function body. The variable holds its
//! target's slot; its reads are `Intrinsic::WeakRead` (`local_expr`), its initializer and the
//! values assigned to it are borrowed, as a weak field's are, and a test does not narrow it
//! (`refuse_fact`): the target may be gone between two reads. A closure capturing one is not
//! lowered yet.

use velt_common::Span;
use velt_syntax::ast;

use super::{FnCx, LocalKind, Want};
use crate::hir::{self, LocalId, StmtKind as S};

impl FnCx<'_, '_> {
    /// `let name: weak T | null = init` (`ty`: the read type `T | null`, checked by
    /// `crate::weak_types`): the initializer borrowed (`null` without one), then the variable.
    pub(super) fn weak_var_decl(
        &mut self,
        v: &ast::VarDecl,
        name: &ast::Ident,
        ty: hir::TyId,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) {
        let init = match &v.init {
            Some(e) => self.expr_coerce(e, ty, Want::Borrow),
            None => self.nothing_value(hir::Nothing::Null, ty, name.span),
        };
        let kind = match v.kind {
            ast::VarKind::Const => LocalKind::Const,
            _ => LocalKind::Let,
        };
        let local = self.declare_local(name, ty, kind);
        self.f.locals[local.0 as usize].weak = true;
        Self::push(
            out,
            S::Let {
                local,
                init: Some(init),
            },
            span,
        );
    }

    /// Is `l` a `weak` local of this function?
    pub(crate) fn is_weak_local(&self, l: LocalId) -> bool {
        self.f.locals.get(l.0 as usize).is_some_and(|d| d.weak)
    }

    /// The read of the `weak` local `l` (of type `ty`, its read type): `Intrinsic::WeakRead`.
    pub(super) fn weak_local_read(&mut self, l: LocalId, ty: hir::TyId, span: Span) -> hir::Expr {
        let place = self.mk(hir::ExprKind::Local(l, hir::UseMode::Borrow), ty, span);
        self.intrinsic(hir::Intrinsic::WeakRead, vec![place], ty, span)
    }
}

/// The note for errors at reads of the `weak` variable `name` that a test would have narrowed.
pub(super) fn weak_local_refusal_note(name: &str) -> String {
    format!(
        "`{name}` is a `weak` variable, whose target may be gone between two reads, so a test does not narrow it: test a `const` copy instead: `const target = {name}; if (target !== null) {{ … }}`"
    )
}
