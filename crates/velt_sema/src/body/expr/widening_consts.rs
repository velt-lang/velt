//! Widening literal types (#2686), as TypeScript's fresh literal types. `const a = c ? "x" :
//! "y"` has type `"x" | "y"`, but those literals widen: a `let` or an object literal's property
//! taken from `a` is a `string`, so it may be given another string later. Another `const` keeps
//! them, and so do `x!`, a conditional or `||` with `a` as an arm, a generic call that gives its
//! argument back (`id(a)`) and an arrow's result (`() => a`): their values widen where `a`
//! would. The `as const` form (`c ? ("x" as const) : ("y" as const)`) keeps its literals
//! everywhere, and so does a value checked against a literal type.
//!
//! Velt's types have no fresh flag, so freshness is tracked on the checked values instead
//! ([`FnCx::fresh_depth`]): a local constant is recorded in `Frame::widening_consts`, a module
//! constant is read from its initializer, and every call and arrow is recorded in
//! `Ctx::widening_values` as it is checked. A call gives widening literals when a type
//! parameter at the top level of its result was inferred from them, whatever the callee's form
//! (`id(a)`, `new B().id(a)`, `B.id(a)`, `unbox({ v: a })`), or when its callee is a function
//! whose calls give them (`(() => a)()`, `nested()()`). A value of widening literals is widened
//! where it is stored (`let w = a`, `{ m: a }`; [`FnCx::widened_ref`]), and a type argument
//! inferred from one widens by TypeScript's rule ([`FnCx::widened_candidate`]).

use velt_syntax::ast;

use super::object_builtins::strip_parens;
use crate::body::locals::frame_lookup;
use crate::body::{FnCx, LocalKind, Want};
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `h`, the checked `e`, widened (`"x" | "y"` to `string`) when `e` gives a value of
    /// widening literal types.
    /// `opt(a)` of `T | undefined` widens its `T` (`string | undefined`).
    pub(crate) fn widened_ref(&mut self, e: &ast::Expr, h: hir::Expr) -> hir::Expr {
        if !self.widening_expr(e) {
            return h;
        }
        let TyKind::Option(inner, n) = *self.cx.ty.kind(h.ty) else {
            return self.widen_value(h);
        };
        let wide = self.cx.widened(inner);
        if wide == inner {
            return h;
        }
        let target = self.cx.ty.option_of(wide, n);
        self.try_coerce(h, target).unwrap_or_else(|h| h)
    }

    /// Does `e` (checked) give a value of widening literal types (module docs)? A literal arm
    /// of a conditional or of `||` is one too, as TypeScript's fresh literal is.
    pub(crate) fn widening_expr(&mut self, e: &ast::Expr) -> bool {
        self.fresh_depth(e) == 1
    }

    /// How `e` (checked) gives widening literals, as tsc's fresh literal types flow: 1 when its
    /// value is of such types, `n + 1` when it is a function whose calls give depth `n` (`() =>
    /// a` is 2), 0 otherwise. A call's depth was recorded when it was checked
    /// ([`Self::note_widening_call`]), an arrow's when its body was.
    pub(crate) fn fresh_depth(&mut self, e: &ast::Expr) -> u8 {
        use ast::BinaryOp as B;
        match &e.kind {
            // `await f()` of an async arrow whose result is of widening literals.
            ast::ExprKind::Paren(x) | ast::ExprKind::NonNull(x) | ast::ExprKind::Await(x) => {
                self.fresh_depth(x)
            }
            ast::ExprKind::Ident(id) if self.widening_name(id) => 1,
            ast::ExprKind::Ident(id) => match self.local_widening_fn(id) {
                Some(d) => d,
                None => self.global_widening_fn(id),
            },
            ast::ExprKind::Cond { then, els, .. } => {
                (self.widening_arm(then) || self.widening_arm(els)) as u8
            }
            ast::ExprKind::Binary {
                op: B::And | B::Or | B::Nullish,
                lhs,
                rhs,
            } => (self.widening_arm(lhs) || self.widening_arm(rhs)) as u8,
            ast::ExprKind::Call { .. } | ast::ExprKind::Arrow { .. } => {
                self.cx.widening_values.get(&e.span).copied().unwrap_or(0)
            }
            _ => 0,
        }
    }

    /// Does `e`, or an element or property value of the array or object literal `e`, give a
    /// value of widening literals? Checked against a type parameter's constraint, it keeps them
    /// where the constraint has literal types (`useK({ kind: a })` with `T extends { kind: "x" |
    /// "y" }`), as a literal does (`constraints`).
    pub(super) fn holds_widening(&mut self, e: &ast::Expr) -> bool {
        match &e.kind {
            ast::ExprKind::Array(xs) => xs.iter().any(|x| self.holds_widening(x)),
            ast::ExprKind::Object(ps) => ps.iter().any(|p| match p {
                ast::ObjectProp::KeyValue(_, v) => self.holds_widening(v),
                ast::ObjectProp::Shorthand(name) => self.widening_name(name),
                _ => false,
            }),
            ast::ExprKind::Paren(inner) => self.holds_widening(inner),
            _ => self.widening_expr(e),
        }
    }

    fn widening_arm(&mut self, e: &ast::Expr) -> bool {
        plain_lit(strip_parens(e)) || self.widening_expr(e)
    }

    /// After the call `e` of `callee` was checked: record what it gives (`fresh_depth`). It
    /// gives widening literals when `inferred` (a type parameter at the top level of its
    /// result was inferred from them: `id(a)`, `new B().id(a)`, `unbox({ v: a })`;
    /// [`Self::gives_widening`]), or what `callee`'s calls give (`(() => a)()`, `nested()()`).
    pub(super) fn note_widening_call(&mut self, e: &ast::Expr, callee: &ast::Expr, inferred: bool) {
        let depth = match inferred {
            true => 1,
            false => self.fresh_depth(callee).saturating_sub(1),
        };
        if depth > 0 {
            self.cx.widening_values.insert(e.span, depth);
        }
    }

    /// Does the argument `e` for a parameter of type `pty` of the generic callable `c` make its
    /// result one of widening literals? So it does when `e` holds them (anywhere: `unbox({ v: a
    /// })`, `apply(() => a)`) for a type parameter of `pty` that is at the top level of the
    /// result and has no primitive constraint: TypeScript infers that parameter as the fresh
    /// literals (with a primitive constraint, as regular ones; `getCovariantInference`).
    pub(super) fn gives_widening(
        &mut self,
        c: &super::args::Callable,
        e: &ast::Expr,
        pty: TyId,
    ) -> bool {
        if c.slot_names.is_empty() || self.fresh_call {
            return false;
        }
        let rets = self.top_params(c.ret);
        if rets.is_empty() {
            return false;
        }
        let mut ps = vec![];
        crate::types::collect_params(&self.cx.ty, pty, &mut ps);
        let kept = ps.into_iter().any(|k| {
            let constraint = c.constraints.get(k as usize).copied().flatten();
            rets.contains(&k) && !self.primitive_constraint(constraint)
        });
        kept && (self.fresh_depth(e) > 0 || self.holds_widening(e))
    }

    /// For a local `id` names (in this function or an enclosing one): the depth (`fresh_depth`)
    /// of the function value it holds, a constant. `None` when `id` names no local.
    fn local_widening_fn(&self, id: &ast::Ident) -> Option<u8> {
        let frames = std::iter::once(&self.f).chain(self.outer.iter().rev());
        for f in frames {
            if let Some(l) = frame_lookup(f, &id.name) {
                if f.kinds[l.0 as usize] != LocalKind::Capture {
                    return Some(f.widening_fn_locals.get(&l).copied().unwrap_or(0));
                }
            }
        }
        None
    }

    /// The depth (`fresh_depth`) of the function value the module constant `id` names holds
    /// (`const f = () => a`), 0 for anything else.
    fn global_widening_fn(&self, id: &ast::Ident) -> u8 {
        let Some(Item::Def(d)) = self.cx.lookup_item_at(self.module, &id.name, id.span) else {
            return 0;
        };
        let Some(DefInfo::Global(g)) = self.cx.info.get(d.0 as usize) else {
            return 0;
        };
        let init = g.src.init.map(strip_parens).filter(|_| !g.mutable);
        let depth = init.and_then(|e| self.cx.widening_values.get(&e.span).copied());
        depth.filter(|d| *d > 1).unwrap_or(0)
    }

    /// The type parameters at the top level of `t`: `T`, a member of `T | U` or `T | undefined`.
    fn top_params(&mut self, t: TyId) -> Vec<u32> {
        match self.cx.ty.kind(t).clone() {
            TyKind::Param(k) => vec![k],
            TyKind::Option(inner, _) => self.top_params(inner),
            _ => match self.cx.union_members(t) {
                Some(ms) => ms.into_iter().flat_map(|m| self.top_params(m)).collect(),
                None => vec![],
            },
        }
    }

    /// Is `c`, a type parameter's constraint, primitive (`T extends string`, `T extends "a" |
    /// 1`)? TypeScript infers such a parameter's literals without widening.
    fn primitive_constraint(&mut self, c: Option<TyId>) -> bool {
        let Some(c) = c else {
            return false;
        };
        let members = self.cx.union_members(c).unwrap_or_else(|| vec![c]);
        let ty = &self.cx.ty;
        members.iter().any(|&t| {
            t == ty.str_
                || t == ty.bool_
                || ty.is_numeric(t)
                || matches!(ty.kind(t), TyKind::Literal(_))
        })
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
    /// a `const` without a written type, initialized by a conditional of plain literals or by
    /// another such constant?
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
            ast::ExprKind::NonNull(next) => match &strip_parens(next).kind {
                ast::ExprKind::Ident(next) => self.widening_global(g.module, next, depth + 1),
                _ => false,
            },
            _ => widening_cond(init),
        }
    }

    /// The type argument `e` (of type `ty`, passed for a parameter of type `pty`) offers. A
    /// value of widening literal types offers their base type (`string`) by TypeScript's rule
    /// (`getCovariantInference`): when a type parameter is at the top level of `pty`, has no
    /// primitive constraint and is not at the top level of the result. So `signal(mode)` is a
    /// `Signal<string>`, but `id(mode)` (`T` → `T`) keeps `"a" | "b"`.
    pub(super) fn widened_candidate(
        &mut self,
        c: &super::args::Callable,
        e: &ast::Expr,
        pty: TyId,
        ty: TyId,
    ) -> TyId {
        if !self.widening_expr(e) {
            return ty;
        }
        let rets = self.top_params(c.ret);
        let widen = self.top_params(pty).into_iter().any(|k| {
            let constraint = c.constraints.get(k as usize).copied().flatten();
            !rets.contains(&k) && !self.primitive_constraint(constraint)
        });
        match widen {
            true => self.cx.widened(ty),
            false => ty,
        }
    }

    /// The initializer `e` of local `name` declared by `v` without a type: a `let` of a value
    /// of widening literals is of their base type (`let w = a` is a `string`).
    pub(crate) fn untyped_init(
        &mut self,
        v: &ast::VarDecl,
        e: &ast::Expr,
        name: &ast::Ident,
    ) -> hir::Expr {
        let h = self.copy_init(e, name, Want::Move);
        match v.kind {
            ast::VarKind::Let => self.widened_ref(e, h),
            _ => h,
        }
    }

    /// The initializer `e` of a module constant (or, `mutable`, a module `let`) without a
    /// written type: a `const`'s conditional of literals keeps them, as a local's does
    /// (`decl_keeping_arms`), and a `let` of a value of widening literals is a `string`.
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
    /// widening literals when `init` is a conditional of plain literals or another value of
    /// widening literals, and the local holds a literal type or a union of them.
    pub(crate) fn note_widening_const(&mut self, v: &ast::VarDecl, name: &ast::Ident) {
        if v.kind != ast::VarKind::Const || v.ty.is_some() {
            return;
        }
        let Some(init) = v.init.as_ref() else {
            return;
        };
        let Some(l) = frame_lookup(&self.f, &name.name) else {
            return;
        };
        let depth = match widening_cond(strip_parens(init)) {
            true => 1,
            false => self.fresh_depth(init),
        };
        if depth > 1 {
            self.f.widening_fn_locals.insert(l, depth);
            return;
        }
        if depth == 0 {
            return;
        }
        let ty = self.f.locals[l.0 as usize].ty;
        if self.cx.widened(ty) != ty {
            self.f.widening_consts.insert(l);
        }
    }
}

/// Is `e` a conditional whose arms keep their literals in a `const` (`kept_arm`), at least one
/// of them a plain literal (not `as const`)?
pub(crate) fn widening_cond(e: &ast::Expr) -> bool {
    matches!(e.kind, ast::ExprKind::Cond { .. })
        && super::ternary::kept_arm(e, true)
        && has_plain_arm(e)
}

fn has_plain_arm(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(inner) => has_plain_arm(inner),
        ast::ExprKind::Cond { then, els, .. } => has_plain_arm(then) || has_plain_arm(els),
        _ => plain_lit(e),
    }
}

/// A literal TypeScript gives a fresh literal type: a string (`plain_str`), a number (`-1`
/// too) or a boolean.
pub(crate) fn plain_lit(e: &ast::Expr) -> bool {
    plain_str(e).is_some()
        || plain_num(e)
        || matches!(e.kind, ast::ExprKind::Lit(ast::Lit::Bool(_)))
}

/// A number literal without a type suffix, or its negation.
pub(crate) fn plain_num(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(
            ast::Lit::Int { suffix: None, .. } | ast::Lit::Float { suffix: None, .. },
        ) => true,
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        } => matches!(
            expr.kind,
            ast::ExprKind::Lit(
                ast::Lit::Int { suffix: None, .. } | ast::Lit::Float { suffix: None, .. }
            )
        ),
        _ => false,
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
