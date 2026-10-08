//! Expression lowering. `expr` yields an operand (aggregates as a place: a local, a borrowed
//! param, a projection, or an owned temporary registered for dropping); `consume` additionally
//! takes ownership of the value for a store, return or owned argument.

use velt_sema::hir::{self, LocalId, TyId, UseMode};

use super::rt::Rt;
use super::sequence::{later_each, Later};
use super::{cint, ice, unit, FnLower, Glue, ScopeKind, Work};
use crate::vir::{self, Const, Operand, Place, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Evaluate an expression whose value will be *owned* by the consumer (not registered for
    /// dropping anywhere).
    pub(super) fn consume(&mut self, e: &hir::Expr) -> Operand {
        use hir::ExprKind as K;
        if let K::Upcast(inner) = &e.kind {
            return self.consume(inner);
        }
        if let K::Downcast(inner) = &e.kind {
            let v = self.consume(inner);
            return self.downcast_value(v, inner.ty);
        }
        let v = self.expr(e);
        if self.dead() || !self.needs_drop(e.ty) {
            return v;
        }
        match &e.kind {
            K::Local(id, UseMode::Move) if self.info[id.0 as usize].droppable => v,
            // String literals are static (cap = 0): bitwise copies are free and never freed.
            K::Lit(_) => v,
            // Moved out of a place: `expr` already transferred (or re-registered) ownership.
            K::Field {
                mode: UseMode::Move,
                ..
            }
            | K::UnwrapSome(_, UseMode::Move)
            | K::UnwrapVariant {
                mode: UseMode::Move,
                ..
            } => match &v {
                Operand::Copy(p) => {
                    self.take_temp(&p.clone());
                    v
                }
                _ => v,
            },
            _ => match &v {
                Operand::Copy(p) if self.take_temp(&p.clone()) => v,
                // A borrowed place used as an owned value: another reference to it (JS).
                _ => {
                    let ty = self.sub(e.ty);
                    self.share_value(v, ty)
                }
            },
        }
    }

    /// Deep copy of a value of concrete type `ty` into a fresh (unregistered) temporary.
    pub(super) fn clone_value(&mut self, v: Operand, ty: TyId) -> Operand {
        if !self.cx.needs_drop(ty) {
            return v;
        }
        let vt = self.cx.ty(ty);
        let src = self.operand_addr(v, vt);
        let out = self.temp(vt);
        let dst = self.addr(Place::local(out));
        match self.cx.kind(ty) {
            hir::TyKind::Str => self.call_rt(Rt::StrClone, vec![src, dst], None),
            _ => {
                let f = self.cx.func(Work::Glue(Glue::Clone, ty));
                self.call(vir::Callee::Func(f), vec![src, dst], None, false);
            }
        }
        Operand::Copy(Place::local(out))
    }

    /// `consume` each of `es` in order, snapshotting a value that a later one may change
    /// (`[xs[0], grow(xs)]` reads `xs[0]` before `grow` reallocates `xs`, #580).
    pub(super) fn consume_each(&mut self, es: &[hir::Expr]) -> Vec<Operand> {
        let later = later_each(es);
        let mut vals = Vec::with_capacity(es.len());
        for (e, later) in es.iter().zip(later) {
            let v = self.consume(e);
            vals.push(self.hold_owned(v, e.ty, later));
        }
        vals
    }

    pub(super) fn expr(&mut self, e: &hir::Expr) -> Operand {
        if self.dead() {
            return unit();
        }
        let prev = self.enter_span(e.span);
        let v = self.expr_at(e);
        self.restore_loc(prev);
        v
    }

    /// `expr` once the location is set.
    fn expr_at(&mut self, e: &hir::Expr) -> Operand {
        use hir::ExprKind as K;
        match &e.kind {
            K::Lit(l) => self.lit(l, e.ty),
            K::Local(id, mode) => self.local_expr(*id, *mode),
            K::Unary { op, expr } => self.unary(*op, expr, e.ty),
            K::Binary { op, lhs, rhs } => {
                if let Some(v) = self.float_param_ordering(*op, lhs, rhs, e.ty) {
                    return v;
                }
                let l = self.expr_held(lhs, Later::of(rhs));
                let r = self.expr(rhs);
                self.binop(*op, l, r, lhs.ty, e.ty)
            }
            K::Logical { op, lhs, rhs } => self.logical(*op, lhs, rhs),
            K::Assign { place, value } => self.assign_expr(place, value),
            K::CompoundAssign { op, place, value } => self.compound_assign(*op, place, value),
            K::Call {
                callee: hir::Callee::Intrinsic(hir::Intrinsic::SourceLocation),
                ..
            } => self.source_location(e.span),
            K::Call { callee, args } => self.call_value(callee, args, e.ty),
            K::Cast(inner) => {
                let v = self.expr(inner);
                let (from, to) = (self.vty(inner.ty), self.vty(e.ty));
                self.cast_to(v, from, to)
            }
            K::If { cond, then, els } => self.if_expr(cond, then, els, e.ty),
            K::Block(b) => self.block_expr(b, e.ty),
            _ => self.expr_m2(e),
        }
    }

    /// M2 expression kinds (objects, enums, arrays, closures, errors).
    fn expr_m2(&mut self, e: &hir::Expr) -> Operand {
        use hir::ExprKind as K;
        match &e.kind {
            K::Global(d) => self.global(*d),
            K::FnRef(d, targs) => self.fn_ref(*d, targs, e.ty),
            K::Field { base, index, mode } => self.field_expr(base, *index, *mode),
            K::Index { base, index, .. } => self.index_expr(base, index),
            K::AdtLit { fields, .. } => self.adt_lit(e.ty, fields),
            K::Variant { variant, args, .. } => self.variant(e.ty, *variant, args),
            K::ArrayLit(es) => self.array_lit(es, e.ty),
            K::Tuple(es) => self.tuple(es, e.ty),
            K::Closure(d) => self.closure(*d, e.ty),
            K::Match { scrutinee, arms } => self.match_expr(scrutinee, arms, e.ty),
            K::WrapSome(inner) => self.wrap_some(inner, e.ty),
            K::UnwrapSome(inner, mode) => self.unwrap_some(inner, *mode, e.ty),
            K::UnwrapVariant {
                expr,
                variant,
                mode,
            } => self.unwrap_variant(expr, *variant, *mode),
            K::New { args, .. } => self.new_object(e.ty, args),
            K::Upcast(inner) => self.expr(inner),
            K::Downcast(inner) => {
                let v = self.expr(inner);
                self.downcast_value(v, inner.ty)
            }
            K::ToDyn { expr, impl_index } => self.make_dyn(expr, *impl_index, e.ty),
            K::Throw(inner) => self.throw(inner),
            K::Await(inner) => self.await_expr(inner),
            k => ice(format_args!("unexpected expression {k:?}")),
        }
    }

    fn local_expr(&mut self, id: LocalId, mode: UseMode) -> Operand {
        if self.info[id.0 as usize].vir.is_none() {
            return unit();
        }
        let p = self.local_place(id);
        if mode == UseMode::Move {
            self.mark_moved(id);
        }
        Operand::Copy(p)
    }

    /// Module-level constant: its (constant) initializer is re-evaluated at each use.
    fn global(&mut self, d: hir::DefId) -> Operand {
        let hir::Def::Global(g) = self.cx.hir.def(d) else {
            ice("global reference to a non-global")
        };
        let saved = std::mem::take(&mut self.targs);
        let v = self.consume(&g.init);
        self.targs = saved;
        if !self.cx.needs_drop(g.ty) {
            return v;
        }
        let vt = self.cx.ty(g.ty);
        let t = self.copy_to_temp(v, vt);
        self.own_temp(t, g.ty);
        Operand::Copy(Place::local(t))
    }

    fn unary(&mut self, op: hir::UnOp, inner: &hir::Expr, ty: TyId) -> Operand {
        let t = self.vty(ty);
        // Fold negative literals (sema encodes `-5` as `Neg(5)`).
        if op == hir::UnOp::Neg {
            match &inner.kind {
                hir::ExprKind::Lit(hir::Lit::Int(n)) if t.is_int() => {
                    return cint(super::operand::wrap_int(-(*n as i128), t), t)
                }
                hir::ExprKind::Lit(hir::Lit::Float(f)) => {
                    return Operand::Const(Const::Float(-f), t)
                }
                _ => {}
            }
        }
        let v = self.expr(inner);
        let op = match op {
            hir::UnOp::Neg => vir::UnOp::Neg,
            hir::UnOp::Not => vir::UnOp::Not,
            hir::UnOp::BitNot => vir::UnOp::BitNot,
        };
        self.rvalue_temp(t, Rvalue::Unary(op, v))
    }

    fn logical(&mut self, op: hir::LogicOp, lhs: &hir::Expr, rhs: &hir::Expr) -> Operand {
        let l = self.expr(lhs);
        let res = self.temp(Ty::Bool);
        self.assign(Place::local(res), Rvalue::Use(l.clone()));
        let rhs_bb = self.new_block();
        let join = self.new_block();
        match op {
            hir::LogicOp::And => self.branch(l, rhs_bb, join),
            hir::LogicOp::Or => self.branch(l, join, rhs_bb),
        }
        self.switch_to(rhs_bb);
        self.push_scope(ScopeKind::Temps);
        let r = self.expr(rhs);
        self.assign(Place::local(res), Rvalue::Use(r));
        self.pop_scope();
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(Place::local(res))
    }

    /// Value-producing `if` (ternary): each branch stores its (owned) value into a join temp.
    fn if_expr(
        &mut self,
        cond: &hir::Expr,
        then: &hir::Expr,
        els: &hir::Expr,
        ty: TyId,
    ) -> Operand {
        let c = self.expr(cond);
        let t = self.vty(ty);
        let res = (t != Ty::Unit).then(|| self.temp(t));
        let then_bb = self.new_block();
        let else_bb = self.new_block();
        let join = self.new_block();
        self.branch(c, then_bb, else_bb);
        for (bb, branch) in [(then_bb, then), (else_bb, els)] {
            self.switch_to(bb);
            self.push_scope(ScopeKind::Temps);
            let v = self.consume(branch);
            if let Some(r) = res {
                self.assign(Place::local(r), Rvalue::Use(v));
            }
            self.pop_scope();
            self.goto(join);
        }
        self.switch_to(join);
        let ty = self.sub(ty);
        self.owned_result(res, ty)
    }

    /// Block expression (`x++`/`++x` encodings, match arms): its value outlives its scope.
    fn block_expr(&mut self, b: &hir::Block, ty: TyId) -> Operand {
        self.push_scope(ScopeKind::Block);
        for s in &b.stmts {
            self.stmt(s);
        }
        let t = self.vty(ty);
        let value = match &b.value {
            Some(v) if t != Ty::Unit => Some(self.consume(v)),
            Some(v) => {
                self.expr_stmt(v);
                None
            }
            None => None,
        };
        // Copy a place value out before the block's scope drops/ends; constants need no copy.
        let res = match value {
            Some(op @ Operand::Copy(_)) => Some(self.copy_to_temp(op, t)),
            other => {
                self.pop_scope();
                return other.unwrap_or_else(unit);
            }
        };
        self.pop_scope();
        let ty = self.sub(ty);
        self.owned_result(res, ty)
    }

    /// A join/result temp: registered for dropping in the enclosing scope if it owns resources.
    pub(super) fn owned_result(&mut self, res: Option<vir::Local>, ty: TyId) -> Operand {
        match res {
            Some(r) => {
                self.own_temp(r, ty);
                Operand::Copy(Place::local(r))
            }
            None => unit(),
        }
    }

    /// A fresh owned temporary holding `v`, registered for dropping as concrete `ty`.
    pub(super) fn own_value(&mut self, v: Operand, ty: TyId) -> Operand {
        let vt = self.cx.ty(ty);
        if vt == Ty::Unit {
            return unit();
        }
        let t = self.copy_to_temp(v, vt);
        self.own_temp(t, ty);
        Operand::Copy(Place::local(t))
    }

    /// Place of a local read in place-expression position (`x = …`, `x.f = …`).
    pub(super) fn local_target(&mut self, id: LocalId) -> Option<Place> {
        self.info[id.0 as usize].vir?;
        Some(self.local_place(id))
    }

    /// Place of an operand that must be a place (a diverging operand yields a dummy).
    pub(super) fn place_of(&mut self, v: Operand, ty: TyId) -> Place {
        let vt = self.cx.ty(ty);
        self.operand_place(v, vt)
    }

    /// Const `true`.
    pub(super) fn ctrue() -> Operand {
        Operand::Const(Const::Bool(true), Ty::Bool)
    }

    /// Assign `v` to `p` (skipped for Unit values).
    pub(super) fn store(&mut self, p: Place, v: Operand) {
        if !matches!(v, Operand::Const(Const::Unit, _)) {
            self.assign(p, Rvalue::Use(v));
        }
    }
}
