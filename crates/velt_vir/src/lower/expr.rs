//! Expression lowering. `expr` yields an operand (aggregates as a place: a local, a borrowed
//! param, a projection, or an owned temporary registered for dropping); `consume` additionally
//! takes ownership of the value for a store, return or owned argument.

use velt_sema::hir::{self, LocalId, TyId, UseMode};

use super::rt::Rt;
use super::{cint, ice, unit, FnLower, Glue, ScopeKind, Work};
use crate::vir::{self, Const, Operand, Place, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Evaluate an expression whose value will be *owned* by the consumer (not registered for
    /// dropping anywhere).
    pub(super) fn consume(&mut self, e: &hir::Expr) -> Operand {
        use hir::ExprKind as K;
        if let K::Upcast(inner) = &e.kind {
            let (from, to) = (self.sub(inner.ty), self.sub(e.ty));
            // An interface upcast builds a new interface value (`expr`, below).
            if self.upcast_slot(from, to).is_none() {
                self.cx.note_proxy_cast(from, to);
                return self.consume(inner);
            }
        }
        if let K::Downcast(inner) = &e.kind {
            let v = self.consume(inner);
            let owned = matches!(inner.kind, K::Local(_, hir::UseMode::Move));
            return self.downcast_value(v, inner.ty, owned);
        }
        if let K::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::SharedRead),
            args,
        } = &e.kind
        {
            return self.shared_read_owned(&args[0]);
        }
        // A frozen value or a part of one, consumed: other tasks still read it, so the consumer
        // gets a deep copy, read in place (frozen.rs).
        if self.frozen_root(e) {
            let v = self.frozen_read(e);
            let ty = self.sub(e.ty);
            return self.clone_value(v, ty);
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
                Operand::Copy(p) if self.take_temp(&p.clone()) => v,
                // Out of a variable that only borrows its value (`const s = this.f`, narrowed):
                // nothing was transferred, so the consumer gets another reference (#1140).
                _ if self.borrowed_root(e) => {
                    let ty = self.sub(e.ty);
                    self.share_value(v, ty)
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

    /// Is the moved part `e` (`x.f`, `x!`, a narrowed member of `x`) rooted at a variable that
    /// does not own its value (a binding by reference, or a borrowed parameter)?
    fn borrowed_root(&self, e: &hir::Expr) -> bool {
        super::place::part_root(e).is_some_and(|id| {
            let info = &self.info[id.0 as usize];
            !info.droppable && info.vir.is_some()
        })
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
        let later = self.later_each(es);
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
                let later = self.later_of(rhs);
                let l = self.expr_held(lhs, later);
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
            K::Call { callee, args } => {
                // `super.m(args)` (`Program::super_calls`), taken by `call_def`.
                let sup = self.cx.hir.super_calls.contains(&e.span);
                let prev = std::mem::replace(&mut self.super_call, sup);
                let v = self.call_value(callee, args, e.ty);
                self.super_call = prev;
                v
            }
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
            K::AdtLit {
                fields,
                converted: c @ (hir::Conversion::Copy | hir::Conversion::ProxyOnly),
                ..
            } => self.converted_lit(e.ty, fields, *c == hir::Conversion::ProxyOnly),
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
            K::Upcast(inner) => {
                let (from, to) = (self.sub(inner.ty), self.sub(e.ty));
                if let Some(slot) = self.upcast_slot(from, to) {
                    let v = self.consume(inner);
                    return self.upcast_value(v, from, to, slot);
                }
                self.cx.note_proxy_cast(from, to);
                self.expr(inner)
            }
            K::Downcast(inner) => {
                let v = self.expr(inner);
                self.downcast_value(v, inner.ty, false)
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

    /// Module-level constant: its (constant) initializer is re-evaluated at each use. (An
    /// element, the length or an iteration of a table reads stored data instead, `table_read`.)
    fn global(&mut self, d: hir::DefId) -> Operand {
        let hir::Def::Global(g) = self.cx.hir.def(d) else {
            ice("global reference to a non-global")
        };
        if let Some(slot) = g.slot {
            // A module-level `let`: its slot, read in place (sema shares what it moves out).
            return Operand::Copy(self.module_slot(slot));
        }
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

    /// `e` when it is a table (a module constant that is an array literal of number or boolean
    /// constants), for a use that only reads it where it stands: an element, the length, an
    /// iteration. Its elements are read-only data emitted once, and the result is the place of
    /// the inline header `{ data, len, cap: 0 }` over them (`cap == 0`: the buffer is not owned, so nothing
    /// frees it), also where arrays of this type are counted boxes: the header never leaves the
    /// code of those three reads, so nothing counts it.
    ///
    /// Only those three uses get it. Anywhere else (an argument, a receiver, a value that is
    /// kept) the constant is evaluated like any other, to a new array. Sema rejects the uses
    /// that could write to a table, those it can follow and those it can't (a function value),
    /// but read-only data must never be reachable from code that writes, so this does not rely
    /// on it.
    ///
    /// `None`, for the initializer to be evaluated, also when the array is empty (nothing to
    /// store) or an element is not one `table_const` folds.
    pub(super) fn table_read(&mut self, e: &hir::Expr) -> Option<Place> {
        let (data, len) = self.table_data(e)?;
        let a = self.cx.array_agg();
        let t = self.temp(Ty::Agg(a));
        self.assign(
            Place::local(t),
            Rvalue::Aggregate(
                a,
                vec![
                    Operand::Const(Const::Static(data), Ty::Ptr),
                    cint(len as i128, Ty::U64),
                    cint(0, Ty::U64),
                ],
            ),
        );
        Some(Place::local(t))
    }

    /// Whether `e` names a table that lowering stores as read-only data (`table_read`).
    pub(super) fn is_table(&mut self, e: &hir::Expr) -> bool {
        self.table_data(e).is_some()
    }

    /// The read-only data of the table `e` names, and its length (stored on first use).
    fn table_data(&mut self, e: &hir::Expr) -> Option<(vir::StaticId, usize)> {
        let hir::ExprKind::Global(d) = e.kind else {
            return None;
        };
        let (data, len) = match self.cx.tables.get(&d) {
            Some(known) => (*known)?,
            None => {
                let hir::Def::Global(g) = self.cx.hir.def(d) else {
                    return None;
                };
                // A module-level `let` (its slot holds the array, which code may replace or
                // modify) is never read from the stored initializer.
                if g.slot.is_some() {
                    self.cx.tables.insert(d, None);
                    return None;
                }
                let stored = self.store_table(g);
                self.cx.tables.insert(d, stored);
                stored?
            }
        };
        Some((data, len))
    }

    /// The elements of table `g` as static data, with their number; `None` when `g` is not a
    /// table lowering stores (`table_read`).
    fn store_table(&mut self, g: &hir::GlobalDef) -> Option<(vir::StaticId, usize)> {
        let hir::ExprKind::ArrayLit(es) = &g.init.kind else {
            return None;
        };
        if es.is_empty() {
            return None;
        }
        let elem = self.elem_ty(g.ty);
        let et = self.cx.ty(elem);
        let size = et.scalar_size().filter(|_| et != Ty::Ptr)? as usize;
        let mut bytes = Vec::with_capacity(es.len() * size);
        for e in es {
            match self.table_const(e, et, 0)? {
                Const::Int(v) => bytes.extend_from_slice(&v.to_le_bytes()[..size]),
                Const::Float(v) if et == Ty::F32 => {
                    bytes.extend_from_slice(&(v as f32).to_le_bytes())
                }
                Const::Float(v) => bytes.extend_from_slice(&v.to_le_bytes()),
                Const::Bool(b) => bytes.push(b as u8),
                _ => return None,
            }
        }
        Some((self.cx.static_bytes(bytes, size as u32), es.len()))
    }

    /// The value of table element `e` as a constant of type `to`: a number or boolean literal,
    /// negated or cast (with `as` semantics), or a module constant that is one. `None` for
    /// anything else (sema's `table_elem` accepts exactly what this folds).
    fn table_const(&mut self, e: &hir::Expr, to: Ty, depth: u32) -> Option<Const> {
        let at = self.vty(e.ty);
        let v = match &e.kind {
            hir::ExprKind::Lit(hir::Lit::Int(n)) if at.is_float() => Const::Float(*n as f64),
            hir::ExprKind::Lit(hir::Lit::Int(n)) if at.is_int() => {
                Const::Int(super::operand::wrap_int(*n as i128, at))
            }
            hir::ExprKind::Lit(hir::Lit::Float(f)) if at.is_float() => Const::Float(*f),
            hir::ExprKind::Lit(hir::Lit::Bool(b)) => Const::Bool(*b),
            hir::ExprKind::Unary {
                op: hir::UnOp::Neg,
                expr,
            } => match self.table_const(expr, at, depth)? {
                Const::Int(v) => Const::Int(super::operand::wrap_int(-v, at)),
                Const::Float(v) => Const::Float(-v),
                _ => return None,
            },
            hir::ExprKind::Cast(inner) => {
                let from = self.vty(inner.ty);
                self.table_const(inner, from, depth)?
            }
            hir::ExprKind::Global(d) if depth < 16 => {
                let hir::Def::Global(g) = self.cx.hir.def(*d) else {
                    return None;
                };
                let from = self.vty(g.ty);
                self.table_const(&g.init, from, depth + 1)?
            }
            _ => return None,
        };
        // To the type asked for, as a cast does it.
        Some(match v {
            Const::Int(v) if to.is_int() => Const::Int(super::operand::wrap_int(v, to)),
            // Through `f64`, as `cast_to` folds it: the `f32` is rounded from that.
            Const::Int(v) if to == Ty::F32 => Const::Float(v as f64 as f32 as f64),
            Const::Int(v) if to.is_float() => Const::Float(v as f64),
            Const::Bool(b) if to.is_int() => Const::Int(b as i128),
            Const::Float(v) if to == Ty::F32 => Const::Float(v as f32 as f64),
            Const::Float(_) if to.is_float() => v,
            Const::Float(v) if to.is_int() => Const::Int(float_to_int(v, to)),
            Const::Bool(_) if to == Ty::Bool => v,
            _ => return None,
        })
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
        // A constant operand (`__intrinsic_holds_null`) decides here: no blocks, and the operand
        // it skips is not lowered (std/prelude/map.vlt keeps one shape of code per `V`).
        let and = op == hir::LogicOp::And;
        match (self.const_cond(lhs), self.const_cond(rhs)) {
            (Some(l), _) if l != and => return Operand::Const(vir::Const::Bool(l), Ty::Bool),
            (Some(_), _) => return self.expr(rhs),
            (None, Some(r)) if r == and => return self.expr(lhs),
            _ => {}
        }
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

    /// The value of a condition known per instantiation without running it: built from
    /// `__intrinsic_holds_null` with `!`, `&&` and `||` (std/prelude/map.vlt). Other constant
    /// conditions are left to velt_opt, so their code keeps its shape.
    pub(super) fn const_cond(&mut self, e: &hir::Expr) -> Option<bool> {
        match &e.kind {
            hir::ExprKind::Call {
                callee: hir::Callee::Intrinsic(hir::Intrinsic::HoldsNull),
                args,
            } => {
                let t = self.sub(args.first()?.ty);
                Some(matches!(
                    self.cx.kind(t),
                    velt_sema::hir::TyKind::Option(..)
                ))
            }
            hir::ExprKind::Call {
                callee: hir::Callee::Intrinsic(hir::Intrinsic::MayHoldPrimitive),
                args,
            } => {
                let t = self.sub(args.first()?.ty);
                Some(!self.cx.object_only(t))
            }
            hir::ExprKind::Unary {
                op: hir::UnOp::Not,
                expr,
            } => self.const_cond(expr).map(|v| !v),
            hir::ExprKind::Logical { op, lhs, rhs } => {
                let and = *op == hir::LogicOp::And;
                match self.const_cond(lhs)? {
                    l if l != and => Some(l),
                    _ => self.const_cond(rhs),
                }
            }
            _ => None,
        }
    }

    /// The test `g(x)` of a condition `g(x) && MayHoldPrimitive(x)` whose `MayHoldPrimitive`
    /// is `false` in this instantiation (sema `primitive_guard`).
    pub(super) fn ruled_out_guard<'e>(&mut self, cond: &'e hir::Expr) -> Option<&'e hir::Expr> {
        let hir::ExprKind::Logical {
            op: hir::LogicOp::And,
            lhs,
            rhs,
        } = &cond.kind
        else {
            return None;
        };
        let guard = matches!(
            rhs.kind,
            hir::ExprKind::Call {
                callee: hir::Callee::Intrinsic(hir::Intrinsic::MayHoldPrimitive),
                ..
            }
        );
        (guard && self.const_cond(rhs) == Some(false)).then_some(&**lhs)
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
        if let Some(v) = self.proxy_update_block(b) {
            return v;
        }
        self.note_scope_ends(b, false);
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

/// `v as to` for an integer type `to`: toward zero, saturating at the type's range, NaN is 0
/// (Rust's `as`, which is the language's).
fn float_to_int(v: f64, to: Ty) -> i128 {
    let bits = to.scalar_size().unwrap_or(8) * 8;
    let (lo, hi) = if to.is_signed() {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    };
    if v.is_nan() {
        0
    } else {
        (v as i128).clamp(lo, hi)
    }
}
