//! Place expressions: field access (struct fields inline, class fields through the object
//! pointer, `shared<T>` through its box), array indexing, option payloads, and assignment /
//! compound assignment to any of them (dropping the old value).

use std::collections::VecDeque;
use std::rc::Rc;

use velt_sema::effects::may_change_memory;
use velt_sema::hir::{self, LocalId, Pat, PatKind, TyId, TyKind, UseMode};

use super::operand::proj;
use super::sequence::may_write;
use super::types::VariantAt;
use super::{ice, unit, FnLower};
use crate::vir::{Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Place of field `index` of a value of concrete type `ty` stored at `base`.
    pub(super) fn field_place(&mut self, base: &Place, ty: TyId, index: u32) -> Place {
        match self.cx.kind(ty) {
            TyKind::Adt(..) if self.cx.is_class(ty) => {
                let obj = self.cx.obj_agg(ty);
                let base = self.retained_hop(base, ty);
                let p = proj(&base, Proj::Deref(Ty::Agg(obj)));
                proj(&p, Proj::Field(self.cx.vir_field(ty, index)))
            }
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                let value = self.content(base, ty);
                proj(&value, Proj::Field(self.cx.vir_field(ty, index)))
            }
            TyKind::Shared(inner) => {
                let bx = self.cx.shared_box(inner);
                let p = proj(base, Proj::Deref(Ty::Agg(bx)));
                let v = proj(&p, Proj::Field(1));
                self.field_place(&v, inner, index)
            }
            k => ice(format_args!("field access on {k:?}")),
        }
    }

    pub(super) fn field_expr(&mut self, base: &hir::Expr, index: u32, mode: UseMode) -> Operand {
        let bty = self.sub(base.ty);
        if mode == UseMode::Move && self.counted_part(base) {
            // Other owners still see the field (semantics stage 2): share it instead.
            let v = self.field_expr(base, index, UseMode::Borrow);
            let fty = self.cx.member_ty(bty, index);
            let s = self.share_value(v, fty);
            return self.own_value(s, fty);
        }
        if self.is_unit_field(bty, index) {
            self.expr(base);
            return unit();
        }
        if mode == UseMode::Move {
            if let hir::ExprKind::Local(id, _) = &base.kind {
                let info = &self.info[id.0 as usize];
                if info.droppable && !info.zero_parts {
                    let lp = self.local_place(*id);
                    let p = self.field_place(&lp, bty, index);
                    self.mark_field_moved(*id, index);
                    return Operand::Copy(p);
                }
            }
            if let Some(id) = member_root(base).filter(|id| self.info[id.0 as usize].droppable) {
                return self.move_member_field(base, bty, index, id);
            }
            if part_root(base).is_some_and(|id| self.info[id.0 as usize].droppable) {
                let v = self.expr(base);
                let bp = self.place_of(v, bty);
                let fp = self.field_place(&bp, bty, index);
                let fty = self.cx.member_ty(bty, index);
                return self.take_part(fp, fty);
            }
        }
        let v = self.expr(base);
        let bp = self.place_of(v, bty);
        let p = self.field_place(&bp, bty, index);
        if mode == UseMode::Move && self.take_temp(&bp) {
            // Moving a field out of an owned temporary: the rest of it is dropped later.
            let fty = self.cx.member_ty(bty, index);
            self.own_rest(bp, bty, Rc::new(moved_field_pat(index, bty)));
            self.own_place(p.clone(), fty);
        }
        Operand::Copy(p)
    }

    /// Move field `index` out of the narrowed union member `base` of local `id`: the union value
    /// is consumed — the field is copied out, the rest of the member dropped now (sema treats the
    /// local as moved from here on).
    fn move_member_field(
        &mut self,
        base: &hir::Expr,
        bty: TyId,
        index: u32,
        id: LocalId,
    ) -> Operand {
        let v = self.expr(base);
        let bp = self.place_of(v, bty);
        let fty = self.cx.member_ty(bty, index);
        let fp = self.field_place(&bp, bty, index);
        let vt = self.cx.ty(fty);
        let out = self.copy_to_temp(Operand::Copy(fp), vt);
        self.drop_rest(bp, bty, &moved_field_pat(index, bty));
        self.mark_moved(id);
        Operand::Copy(Place::local(out))
    }

    /// Move the value at `p` (a part of a local that stays owned: `x.a.b`, the payload of a
    /// narrowed `x.f`, a field of a local with a drop flag) into a fresh temporary and leave the
    /// all-zero value behind, which drops as nothing: the local's own drop, which may run on any
    /// path later, then skips the moved part (sema forbids using it again until reassigned).
    fn take_part(&mut self, p: Place, ty: TyId) -> Operand {
        let vt = self.cx.ty(ty);
        let out = self.copy_to_temp(Operand::Copy(p.clone()), vt);
        let zero = self.zero_value(vt);
        self.assign(p, Rvalue::Use(zero));
        Operand::Copy(Place::local(out))
    }

    /// Is field `index` of `ty` a zero-sized `void` field (nothing to read)?
    fn is_unit_field(&mut self, ty: TyId, index: u32) -> bool {
        let base = match self.cx.kind(ty) {
            TyKind::Shared(inner) => inner,
            _ => ty,
        };
        let field = match self.cx.kind(base) {
            TyKind::Adt(..) => self.cx.adt_field_tys(base)[index as usize],
            TyKind::Tuple(es) => es[index as usize],
            _ => return false,
        };
        self.cx.is_unit(field)
    }

    pub(super) fn index_expr(&mut self, base: &hir::Expr, index: &hir::Expr) -> Operand {
        let bty = self.sub(base.ty);
        let v = self.expr(base);
        let arr = self.place_of(v, bty);
        let arr = self.content(&arr, bty);
        let i = self.expr(index);
        let ity = self.vty(index.ty);
        Operand::Copy(self.elem_place_checked(&arr, bty, i, ity))
    }

    /// Place of the payload of an option value at `p` (the value itself for null-niche options,
    /// and for flag-only options, whose zero-sized payload is never read).
    pub(super) fn some_payload(&mut self, p: &Place, opt: TyId) -> Place {
        match self.cx.ty(opt) {
            Ty::Ptr | Ty::Bool => p.clone(),
            _ => proj(p, Proj::Field(1)),
        }
    }

    /// The payload (of type `ty`) of the option `inner`. When `ty` is the option type itself
    /// (a generic `U | null` at a nullable `U`, `TyTable::intern`), that is the value itself.
    pub(super) fn unwrap_some(&mut self, inner: &hir::Expr, mode: UseMode, ty: TyId) -> Operand {
        let oty = self.sub(inner.ty);
        let pty = if self.sub(ty) == oty {
            oty
        } else {
            let TyKind::Option(pty) = self.cx.kind(oty) else {
                ice("unwrap of a non-option")
            };
            pty
        };
        if mode == UseMode::Move && self.through_counted(inner, oty) {
            let v = self.unwrap_some(inner, UseMode::Borrow, ty);
            let s = self.share_value(v, pty);
            return self.own_value(s, pty);
        }
        let v = self.expr(inner);
        let p = self.place_of(v, oty);
        self.check_narrowed_field(inner, &p, oty);
        let payload = if pty == oty {
            p.clone()
        } else {
            self.some_payload(&p, oty)
        };
        self.move_payload(inner, &p, &payload, pty, mode)
            .unwrap_or(Operand::Copy(payload))
    }

    /// Sema unwraps a narrowed local directly (nothing can change it unseen) and a narrowed
    /// field path (`node.left` after `node.left !== null`) through this check: a call since
    /// the test may have set the field to null, which panics here instead of reading `null`.
    fn check_narrowed_field(&mut self, inner: &hir::Expr, p: &Place, oty: TyId) {
        if matches!(inner.kind, hir::ExprKind::Local(..)) || self.dead() {
            return;
        }
        let some = self.option_is_some(p, oty);
        let (ok, null) = (self.new_block(), self.new_block());
        self.branch(some, ok, null);
        self.switch_to(null);
        let msg = format!(
            "a narrowed field was set to null before this read{}",
            self.panic_suffix()
        );
        let msg = self.str_lit(&msg);
        let a = self.operand_addr(msg, Ty::Agg(crate::vir::STR_AGG));
        self.call_rt(super::rt::Rt::Panic, vec![a], None);
        self.switch_to(ok);
    }

    /// Payload of the (flow-narrowed) union value `inner` known to be in `variant`.
    pub(super) fn unwrap_variant(
        &mut self,
        inner: &hir::Expr,
        variant: u32,
        mode: UseMode,
    ) -> Operand {
        let ety = self.sub(inner.ty);
        let at = self.variant_at(inner.ty, variant, ety);
        if mode == UseMode::Move && self.through_counted(inner, ety) {
            let v = self.unwrap_variant(inner, variant, UseMode::Borrow);
            let pty = match at {
                VariantAt::Index(i) => self.cx.variant_tys(ety, i)[0],
                VariantAt::Whole => ety,
            };
            let s = self.share_value(v, pty);
            return self.own_value(s, pty);
        }
        let v = self.expr(inner);
        let p = self.place_of(v, ety);
        let (payload, pty) = match at {
            VariantAt::Index(i) => self.variant_part(&p, ety, i, 0),
            // The union collapsed to this member: the value itself.
            VariantAt::Whole => (p.clone(), ety),
        };
        if self.cx.is_unit(pty) {
            // A zero-sized payload (a literal member like `"mid"`) has no field to read.
            return unit();
        }
        self.move_payload(inner, &p, &payload, pty, mode)
            .unwrap_or(Operand::Copy(payload))
    }

    /// Moving the payload out of an option / union moves all it owns: a local the payload
    /// belongs to is moved as a whole; an owned temporary hands the payload on. Returns the
    /// payload's new home when it was taken out of a part of a local.
    fn move_payload(
        &mut self,
        inner: &hir::Expr,
        p: &Place,
        payload: &Place,
        pty: TyId,
        mode: UseMode,
    ) -> Option<Operand> {
        if mode != UseMode::Move {
            return None;
        }
        match unwrap_root(inner) {
            Some(id) => self.mark_moved(id),
            None if part_root(inner).is_some_and(|id| self.info[id.0 as usize].droppable) => {
                // The payload of an option / union inside a local (`x.f!`): it is copied out
                // and the whole option becomes null / the union its zero value (`take_part`).
                let vt = self.cx.ty(pty);
                let out = self.copy_to_temp(Operand::Copy(payload.clone()), vt);
                let ot = self.vty(inner.ty);
                let zero = self.zero_value(ot);
                self.assign(p.clone(), Rvalue::Use(zero));
                return Some(Operand::Copy(Place::local(out)));
            }
            None => {
                if self.take_temp(p) {
                    self.own_place(payload.clone(), pty);
                }
            }
        }
        None
    }

    /// Evaluate a place expression (assignment target).
    pub(super) fn place_expr(&mut self, e: &hir::Expr) -> Place {
        self.place_expr_with(e, &mut VecDeque::new())
    }

    /// Evaluate the index expressions of place `e` (outermost base first) into temporaries, so
    /// they run before an assignment's right-hand side as in JS (`xs[f()] = g()` calls `f`
    /// first) while the element places themselves are formed only after it (the right-hand
    /// side may grow the array).
    fn place_indices(&mut self, e: &hir::Expr, out: &mut VecDeque<Operand>) {
        use hir::ExprKind as K;
        match &e.kind {
            K::Field { base, .. }
            | K::UnwrapSome(base, _)
            | K::UnwrapVariant { expr: base, .. }
            | K::Downcast(base) => self.place_indices(base, out),
            K::Index { base, index, .. } => {
                self.place_indices(base, out);
                let i = self.expr(index);
                out.push_back(self.freeze(i, index.ty));
            }
            _ => {}
        }
    }

    /// `place_expr` with the index operands `pre` already evaluated by `place_indices`.
    fn place_expr_with(&mut self, e: &hir::Expr, pre: &mut VecDeque<Operand>) -> Place {
        use hir::ExprKind as K;
        match &e.kind {
            K::Local(id, _) => self
                .local_target(*id)
                .unwrap_or_else(|| ice("place of a Unit local")),
            K::Field { base, index, .. } => {
                let bty = self.sub(base.ty);
                let bp = self.place_expr_with(base, pre);
                self.field_place(&bp, bty, *index)
            }
            K::Index { base, index, .. } => {
                let bty = self.sub(base.ty);
                let arr = self.place_expr_with(base, pre);
                let arr = self.content(&arr, bty);
                let i = match pre.pop_front() {
                    Some(i) => i,
                    None => self.expr(index),
                };
                let ity = self.vty(index.ty);
                let prev = self.enter_span(e.span);
                let p = self.elem_place_checked(&arr, bty, i, ity);
                self.restore_loc(prev);
                p
            }
            K::UnwrapSome(inner, _) => {
                let oty = self.sub(inner.ty);
                let p = self.place_expr_with(inner, pre);
                self.check_narrowed_field(inner, &p, oty);
                if self.sub(e.ty) == oty {
                    p
                } else {
                    self.some_payload(&p, oty)
                }
            }
            K::UnwrapVariant { expr, variant, .. } => {
                let ety = self.sub(expr.ty);
                let p = self.place_expr_with(expr, pre);
                match self.variant_at(expr.ty, *variant, ety) {
                    VariantAt::Index(i) => self.variant_part(&p, ety, i, 0).0,
                    VariantAt::Whole => p,
                }
            }
            K::Downcast(inner) => {
                let p = self.place_expr_with(inner, pre);
                self.downcast_place(p, inner.ty)
            }
            _ => {
                let v = self.expr(e);
                let ty = self.sub(e.ty);
                self.place_of(v, ty)
            }
        }
    }

    /// `place = value`: evaluate the target's indices, then the new value; drop the old one,
    /// store.
    pub(super) fn assign_expr(&mut self, place: &hir::Expr, value: &hir::Expr) -> Operand {
        if let Some(done) = self.str_append(place, value) {
            return done;
        }
        let mut pre = VecDeque::new();
        self.place_indices(place, &mut pre);
        let v = self.consume(value);
        if self.dead() {
            return unit();
        }
        if let hir::ExprKind::Local(id, _) = &place.kind {
            if let Some(p) = self.local_target(*id) {
                let v = self.detach(v, place.ty);
                self.drop_old(*id);
                self.store(p, v);
                self.mark_init(*id);
            }
            return unit();
        }
        let refill = self.refilled_field(place);
        let ty = self.sub(place.ty);
        let shared = self.through_counted(place, ty);
        let p = self.place_expr_with(place, &mut pre);
        let flag = self.presence_place(place, &p);
        if shared && self.cx.needs_drop(ty) {
            // Other owners see the place: store first, then drop the old value (its `dispose`
            // may reach the place's container and must find it consistent).
            let vt = self.cx.ty(ty);
            let v = self.detach(v, place.ty);
            let old = self.copy_to_temp(Operand::Copy(p.clone()), vt);
            self.store(p, v);
            self.drop_glue(Place::local(old), ty);
        } else if refill.is_none() && self.cx.needs_drop(ty) {
            let v = self.detach(v, place.ty);
            self.drop_glue(p.clone(), ty);
            self.store(p, v);
        } else {
            self.store(p, v);
        }
        // A write to a `presence` field makes it present.
        if let Some(fp) = flag {
            self.assign(fp, Rvalue::Use(FnLower::ctrue()));
        }
        unit()
    }

    /// The presence flag of the `presence` field the HIR `place` (at VIR place `p`) names.
    pub(super) fn presence_place(&mut self, place: &hir::Expr, p: &Place) -> Option<Place> {
        let hir::ExprKind::Field { base, index, .. } = &place.kind else {
            return None;
        };
        let bt = self.sub(base.ty);
        let slot = self.cx.presence_slot(bt, *index)?;
        let mut fp = p.clone();
        match fp.proj.last_mut() {
            Some(Proj::Field(f)) => *f = slot,
            _ => ice("presence of a field place without a field projection"),
        }
        Some(fp)
    }

    /// Assigning a field that was moved out of a local re-initializes it (nothing to drop).
    fn refilled_field(&mut self, place: &hir::Expr) -> Option<(LocalId, u32)> {
        let hir::ExprKind::Field { base, index, .. } = &place.kind else {
            return None;
        };
        let hir::ExprKind::Local(id, _) = &base.kind else {
            return None;
        };
        let moved = &mut self.info[id.0 as usize].moved_fields;
        let pos = moved.iter().position(|f| f == index)?;
        moved.remove(pos);
        Some((*id, *index))
    }

    /// Before the old value of a droppable place is dropped, copy a new value that is still a
    /// reference into memory (e.g. a field moved out of the old value) into its own temporary.
    fn detach(&mut self, v: Operand, ty: TyId) -> Operand {
        if !self.needs_drop(ty) {
            return v;
        }
        match v {
            Operand::Copy(p) if !p.proj.is_empty() => {
                let vt = self.vty(ty);
                Operand::Copy(Place::local(self.copy_to_temp(Operand::Copy(p), vt)))
            }
            v => v,
        }
    }

    pub(super) fn compound_assign(
        &mut self,
        op: hir::BinOp,
        place: &hir::Expr,
        value: &hir::Expr,
    ) -> Operand {
        if op == hir::BinOp::Add {
            if let Some(done) = self.str_append_compound(place, value) {
                return done;
            }
        }
        let local = match &place.kind {
            hir::ExprKind::Local(id, _) => Some(*id),
            _ => None,
        };
        let pty = self.sub(place.ty);
        // Through a counted object, or with a right-hand side that may change the container
        // (`xs[0] += grow(xs)` reallocates `xs`): read the element, evaluate the right-hand
        // side, then form the element's address again for the write (#580).
        if local.is_none() && (self.through_counted(place, pty) || may_change_memory(value)) {
            return self.compound_assign_shared(op, place, value, pty);
        }
        let p = match local {
            Some(id) => match self.local_target(id) {
                Some(p) => p,
                None => {
                    self.expr(value);
                    return unit();
                }
            },
            None => self.place_expr(place),
        };
        let mut l = Operand::Copy(p.clone());
        if may_write(value) {
            l = self.freeze(l, place.ty);
        }
        let r = self.expr(value);
        // On strings (`s += t`) this is a concat into a fresh temp that replaces the old value.
        let v = self.binop(op, l, r, place.ty, place.ty);
        let ty = self.sub(place.ty);
        let owns = self.cx.needs_drop(ty);
        let v = if owns { self.take_owned(v) } else { v };
        match local {
            Some(id) => {
                self.drop_old(id);
                self.assign(p, Rvalue::Use(v));
                self.mark_init(id);
            }
            None => {
                if owns {
                    self.drop_glue(p.clone(), ty);
                }
                self.assign(p, Rvalue::Use(v));
            }
        }
        unit()
    }

    /// `place op= value` where other owners may reach `place` (through a counted object): the
    /// right-hand side may change or free what `place` points to, so the old value is shared
    /// before it runs and the place is formed again afterwards (with the same indices).
    fn compound_assign_shared(
        &mut self,
        op: hir::BinOp,
        place: &hir::Expr,
        value: &hir::Expr,
        ty: TyId,
    ) -> Operand {
        let mut pre = VecDeque::new();
        self.place_indices(place, &mut pre);
        let first = self.place_expr_with(place, &mut pre.clone());
        let l = self.share_value(Operand::Copy(first), ty);
        let l = self.own_value(l, ty);
        let r = self.expr(value);
        let v = self.binop(op, l, r, place.ty, place.ty);
        let v = match self.cx.needs_drop(ty) {
            true => self.take_owned(v),
            false => v,
        };
        let vt = self.cx.ty(ty);
        let v = Operand::Copy(Place::local(self.copy_to_temp(v, vt)));
        let p = self.place_expr_with(place, &mut pre);
        let old = self.copy_to_temp(Operand::Copy(p.clone()), vt);
        self.assign(p, Rvalue::Use(v));
        self.drop_glue(Place::local(old), ty);
        unit()
    }

    /// Take ownership of an operand known to be a freshly registered owned temporary.
    pub(super) fn take_owned(&mut self, v: Operand) -> Operand {
        if let Operand::Copy(p) = &v {
            let owned = self.dead() || self.take_temp(&p.clone());
            if !owned {
                ice("expected an owned temporary");
            }
        }
        v
    }
}

/// Pattern "field `index` was moved out" used to drop the rest of an owned temporary.
fn moved_field_pat(index: u32, ty: TyId) -> Pat {
    let moved = Pat {
        kind: PatKind::Binding(LocalId(u32::MAX), UseMode::Move),
        ty,
        span: velt_common::Span::DUMMY,
    };
    Pat {
        kind: PatKind::Adt {
            fields: vec![(index, moved)],
        },
        ty,
        span: velt_common::Span::DUMMY,
    }
}

/// The local a union member read (an `UnwrapSome` / `UnwrapVariant` chain containing an
/// `UnwrapVariant`) is rooted at.
pub(super) fn member_root(e: &hir::Expr) -> Option<LocalId> {
    match &e.kind {
        hir::ExprKind::UnwrapVariant { expr, .. } => unwrap_root(expr),
        hir::ExprKind::UnwrapSome(base, _) | hir::ExprKind::Downcast(base) => member_root(base),
        _ => None,
    }
}

/// The local an option / union payload projection chain (`UnwrapSome` / `UnwrapVariant`) is
/// rooted at.
pub(super) fn unwrap_root(e: &hir::Expr) -> Option<LocalId> {
    match &e.kind {
        hir::ExprKind::Local(id, _) => Some(*id),
        hir::ExprKind::UnwrapSome(base, _)
        | hir::ExprKind::UnwrapVariant { expr: base, .. }
        | hir::ExprKind::Downcast(base) => unwrap_root(base),
        _ => None,
    }
}

/// The local a chain of field / payload projections (`x`, `x.a.b`, `x.f!`) is rooted at.
pub(super) fn part_root(e: &hir::Expr) -> Option<LocalId> {
    match &e.kind {
        hir::ExprKind::Local(id, _) => Some(*id),
        hir::ExprKind::Field { base, .. }
        | hir::ExprKind::UnwrapSome(base, _)
        | hir::ExprKind::UnwrapVariant { expr: base, .. }
        | hir::ExprKind::Downcast(base) => part_root(base),
        _ => None,
    }
}
