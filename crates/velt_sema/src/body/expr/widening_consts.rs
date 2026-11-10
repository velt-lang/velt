//! Widening literal types (#2686). `const a = c ? "x" : "y"` has type `"x" | "y"`, as in
//! TypeScript, but those literals widen: a `let`, an object literal's property and an inferred
//! function result taken from `a` are `string`s, so they may be given another string later. The
//! `as const` form (`c ? ("x" as const) : ("y" as const)`) keeps its literals everywhere.
//!
//! Such constants are found by their declarations: a local one is recorded in
//! `Frame::widening_consts`, a module constant is read from its initializer. A value written as
//! one of them (`let w = a`, `{ m: a }`, `return a`) is widened where it is stored
//! ([`FnCx::widened_ref`]).

use velt_syntax::ast;

use super::object_builtins::strip_parens;
use crate::body::locals::frame_lookup;
use crate::body::{FnCx, LocalKind, Want};
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir;

impl FnCx<'_, '_> {
    /// `h`, the checked `e`, widened (`"x" | "y"` to `string`) when `e` names a constant of
    /// widening literals.
    pub(crate) fn widened_ref(&mut self, e: &ast::Expr, h: hir::Expr) -> hir::Expr {
        match self.widening_ref(e) {
            true => self.widen_value(h),
            false => h,
        }
    }

    /// Does `e` name a constant of widening literals (module docs)?
    pub(crate) fn widening_ref(&self, e: &ast::Expr) -> bool {
        match &strip_parens(e).kind {
            ast::ExprKind::Ident(id) => self.widening_name(id),
            _ => false,
        }
    }

    /// Does `id` name a constant of widening literals?
    pub(crate) fn widening_name(&self, id: &ast::Ident) -> bool {
        let frames = std::iter::once(&self.f).chain(self.outer.iter().rev());
        for f in frames {
            if let Some(l) = frame_lookup(f, &id.name) {
                if f.kinds[l.0 as usize] != LocalKind::Capture {
                    return f.widening_consts.contains(&l);
                }
            }
        }
        self.widening_global(self.module, id, 0)
    }

    /// Is the module item `id` names (from module `module`) a constant of widening literals:
    /// a `const` without a written type, initialized by a conditional of plain string literals or
    /// by another such constant?
    fn widening_global(&self, module: usize, id: &ast::Ident, depth: u32) -> bool {
        let Some(Item::Def(d)) = self.cx.lookup_item_at(module, &id.name, id.span) else {
            return false;
        };
        let Some(DefInfo::Global(g)) = self.cx.info.get(d.0 as usize) else {
            return false;
        };
        if g.mutable || g.src.ann.is_some() || g.src.owner.is_some() || depth > 8 {
            return false;
        }
        let Some(init) = g.src.init.map(strip_parens) else {
            return false;
        };
        match &init.kind {
            ast::ExprKind::Ident(next) => self.widening_global(g.module, next, depth + 1),
            _ => widening_cond(init),
        }
    }

    /// The type argument `e` (of type `ty`, passed for a parameter of type `pty`) offers:
    /// a constant of widening literals offers their base type (`string`) to type parameters
    /// without a constraint, as TypeScript widens such candidates (`signal(mode)` is a
    /// `Signal<string>`).
    pub(super) fn widened_candidate(
        &mut self,
        c: &super::args::Callable,
        e: &ast::Expr,
        pty: hir::TyId,
        ty: hir::TyId,
    ) -> hir::TyId {
        if !self.widening_ref(e) {
            return ty;
        }
        let mut names = vec![];
        crate::types::collect_params(&self.cx.ty, pty, &mut names);
        let constrained = names
            .iter()
            .any(|&k| c.constraints.get(k as usize).is_some_and(Option::is_some));
        match constrained {
            true => ty,
            false => self.cx.widened(ty),
        }
    }

    /// The initializer `e` of a module constant (or, `mutable`, a module `let`) without a
    /// written type: a `const`'s conditional of literals keeps them, as a local's does
    /// (`decl_keeping_arms`), and a `let` of a constant of widening literals is a `string`.
    pub(crate) fn untyped_global_init(&mut self, e: &ast::Expr, mutable: bool) -> hir::Expr {
        let cond = Some(strip_parens(e)).filter(|c| matches!(c.kind, ast::ExprKind::Cond { .. }));
        let outer = std::mem::replace(&mut self.kept_arms, cond.map(|c| c.span));
        let plain = std::mem::replace(&mut self.kept_plain_arms, !mutable);
        let h = self.expr(e, None, Want::Move);
        (self.kept_arms, self.kept_plain_arms) = (outer, plain);
        match mutable {
            true => self.widened_ref(e, h),
            false => h,
        }
    }

    /// After `const name = init` (no written type) was declared: record `name` as a constant of
    /// widening literals when `init` is a conditional of plain string literals or another such
    /// constant, and the local holds a union of string literals.
    pub(crate) fn note_widening_const(&mut self, v: &ast::VarDecl, name: &ast::Ident) {
        if v.kind != ast::VarKind::Const || v.ty.is_some() {
            return;
        }
        let Some(init) = v.init.as_ref() else {
            return;
        };
        if !widening_cond(strip_parens(init)) && !self.widening_ref(init) {
            return;
        }
        let Some(l) = frame_lookup(&self.f, &name.name) else {
            return;
        };
        let ty = self.f.locals[l.0 as usize].ty;
        if self.string_literals(ty).is_some() {
            self.f.widening_consts.insert(l);
        }
    }
}

/// Is `e` a conditional whose arms keep their literals in a `const` (`kept_arm`), at least one
/// of them a plain string literal (not `as const`)?
pub(crate) fn widening_cond(e: &ast::Expr) -> bool {
    matches!(e.kind, ast::ExprKind::Cond { .. })
        && super::ternary::kept_arm(e, true)
        && has_plain_arm(e)
}

fn has_plain_arm(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(inner) => has_plain_arm(inner),
        ast::ExprKind::Cond { then, els, .. } => has_plain_arm(then) || has_plain_arm(els),
        _ => plain_str(e).is_some(),
    }
}

/// The string of a plain string literal arm: `"a"`, or a template literal without
/// substitutions (`` `a` ``), which TypeScript types as the same literal.
pub(crate) fn plain_str(e: &ast::Expr) -> Option<&str> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(s.as_str()),
        ast::ExprKind::Template { quasis, exprs } if exprs.is_empty() && quasis.len() == 1 => {
            Some(quasis[0].as_str())
        }
        _ => None,
    }
}
