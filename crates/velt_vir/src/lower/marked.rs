//! Prototype for #2730 (not for merge): objects reachable from written shared state are
//! *marked* at run time (bit `MARKED` of their count word), as Lean 4 marks thread-shared
//! objects. Only the types module state can reach (`Cx::markable`, a walk over static types
//! from the module state tuple) pay anything:
//! - their count operations branch on the bit: plain when clear, atomic (`velt_rt_proto_rc_*`)
//!   when set (rc.rs `retain_of`, `release_of`, `unique_of`);
//! - a field access of a marked class object takes the module lock for the stretch
//!   (`velt_rt_proto_shared_lock`, place.rs `field_place`); releases never take it;
//! - a store of a value into a marked object (a field, an element of an array held in a field,
//!   a push onto one) marks what the value reaches (`velt_rt_proto_mark`), and so does the
//!   initialization of a module slot.
//!
//! Programs without module state build exactly as before: the set is empty.

use std::rc::Rc;

use rustc_hash::FxHashSet;
use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::glue::Glue;
use super::rt::Rt;
use super::{cfunc, cint, Cx, FnLower, Work};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

/// The marked bit of a count word (velt_rt `module_state::MARKED`).
pub(super) const MARKED: i128 = 1 << 60;

/// The types whose values module state can reach.
#[derive(Default)]
pub(super) struct Markable {
    types: FxHashSet<TyId>,
    /// Class hierarchies (by root) with subclasses: any class of the hierarchy.
    roots: FxHashSet<DefId>,
}

impl Cx<'_> {
    fn markable_set(&mut self) -> Rc<Markable> {
        if let Some(m) = &self.markable {
            return m.clone();
        }
        let mut m = Markable::default();
        let state = self.hir.module_state.as_ref().map(|s| s.ty);
        if let (Some(state), None) = (state, std::env::var_os("VELT_PROTO_NOMARK")) {
            let mut work = vec![state];
            while let Some(t) = work.pop() {
                let t = self.ph_rep(t);
                if !m.types.insert(t) {
                    continue;
                }
                match self.kind(t) {
                    // Open problem: what a function or interface value holds is not followed.
                    TyKind::Closure(_) | TyKind::FnPtr { .. } | TyKind::Dyn(..) => {}
                    TyKind::Array(e) | TyKind::Option(e, _) | TyKind::Shared(e) => work.push(e),
                    TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => {
                        work.extend([a, b]);
                    }
                    TyKind::Adt(d, args) => {
                        if self.is_class(t) {
                            let root = self.class_root(d);
                            if root != d || self.has_subclasses(d) {
                                m.roots.insert(root);
                            }
                            work.extend(self.adt_field_tys(t));
                        }
                        work.extend(self.part_types(t));
                        work.extend(args);
                    }
                    _ => work.extend(self.part_types(t)),
                }
            }
        }
        let m = Rc::new(m);
        self.markable = Some(m.clone());
        m
    }

    /// May a value of type `t` be marked (reached from module state)?
    pub(super) fn markable(&mut self, t: TyId) -> bool {
        let m = self.markable_set();
        if m.types.is_empty() {
            return false;
        }
        let t = self.ph_rep(t);
        if m.types.contains(&t) {
            return true;
        }
        match self.raw_kind(t) {
            TyKind::Adt(d, _) if self.is_class(t) => {
                let d = *d;
                m.roots.contains(&self.class_root(d))
            }
            _ => false,
        }
    }
}

impl FnLower<'_, '_> {
    /// The marked bit of the counted object `p` (a `Bool`).
    pub(super) fn is_marked(&mut self, p: Operand) -> Operand {
        let c = self.count_place(p);
        let b = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, Operand::Copy(c), cint(MARKED, Ty::U64)),
        );
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, b, cint(0, Ty::U64)))
    }

    /// `marked` when the counted object `p` is marked, else `plain`.
    pub(super) fn marked_split(
        &mut self,
        p: Operand,
        marked: impl FnOnce(&mut Self),
        plain: impl FnOnce(&mut Self),
    ) {
        let m = self.is_marked(p);
        let (mk, pl, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(m, mk, pl);
        self.switch_to(mk);
        marked(self);
        self.goto(join);
        self.switch_to(pl);
        plain(self);
        self.goto(join);
        self.switch_to(join);
    }

    /// A field of the class object at `base` (a place holding its pointer) of type `ty` is
    /// accessed: take the lock for the stretch when the object is marked.
    pub(super) fn marked_access(&mut self, base: &Place, ty: TyId) {
        if self.glue || !self.cx.counted(ty) || !self.cx.markable(ty) {
            return;
        }
        let p = Operand::Copy(base.clone());
        let m = self.is_marked(p);
        let (lock, join) = (self.new_block(), self.new_block());
        self.branch(m, lock, join);
        self.switch_to(lock);
        self.call_rt(Rt::ProtoSharedLock, vec![], None);
        self.goto(join);
        self.switch_to(join);
    }

    /// Mark what the `ty` value at `place` reaches (stored into shared state).
    pub(super) fn mark_value(&mut self, place: Place, ty: TyId) {
        if self.cx.proxiable(ty) {
            return;
        }
        let target = match self.cx.kind(ty) {
            _ if self.cx.counted(ty) => Some(ty),
            TyKind::Option(e, _) if self.cx.ty(ty) == Ty::Ptr && self.cx.counted(e) => Some(e),
            _ => None,
        };
        if let Some(c) = target {
            if self.cx.proxiable(c) {
                return;
            }
            let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place)));
            let tf = self.mark_trace(c);
            self.call_rt(Rt::ProtoMark, vec![p, tf], None);
            return;
        }
        if self.cx.weak_refs_inline(None, ty).is_empty() {
            return;
        }
        // An inline value holding counted objects: its trace glue visits them.
        let a = self.addr(place);
        let g = cfunc(self.cx.func(Work::Glue(Glue::TraceIn, ty)));
        self.call_rt(Rt::ProtoMarkIn, vec![a, g], None);
    }

    /// The trace glue the marking walk follows from a counted object of type `c` (null when it
    /// refers to nothing counted, or is opaque: a class with a vtable, an open problem).
    fn mark_trace(&mut self, c: TyId) -> Operand {
        if !self.cx.traceable(c) || self.cx.weak_refs_of(None, c).is_empty() {
            return cint(0, Ty::Ptr);
        }
        cfunc(self.cx.func(Work::Glue(Glue::Trace, c)))
    }

    /// The class object a store into `place` writes into, when it may be marked and is held in
    /// a local (`o.f = v`, `this.items[i] = v`, `m.entryValues.push(v)`): its HIR expression.
    pub(super) fn marked_owner<'e>(&mut self, place: &'e hir::Expr) -> Option<&'e hir::Expr> {
        let base = match &place.kind {
            hir::ExprKind::Field { base, .. } => base,
            hir::ExprKind::Index { base, .. } => match &base.kind {
                hir::ExprKind::Field { base, .. } => base,
                _ => return None,
            },
            _ => return None,
        };
        let bty = self.sub(base.ty);
        let pure = match &base.kind {
            hir::ExprKind::Local(..) => true,
            hir::ExprKind::UnwrapSome(inner, _) | hir::ExprKind::Downcast(inner) => {
                matches!(inner.kind, hir::ExprKind::Local(..))
            }
            _ => false,
        };
        (pure && self.cx.is_class(bty) && self.cx.counted(bty) && self.cx.markable(bty))
            .then_some(&**base)
    }

    /// After a store of a `ty` value at `stored` into the object `owner` (a local): mark the
    /// value when the object is marked.
    pub(super) fn mark_if_owner_marked(&mut self, owner: &hir::Expr, stored: Place, ty: TyId) {
        let target = self.cx.counted(ty)
            || matches!(self.cx.kind(ty), TyKind::Option(e, _) if self.cx.counted(e))
            || !self.cx.weak_refs_inline(None, ty).is_empty();
        if !target {
            return;
        }
        let op = self.place_expr(owner);
        let p = Operand::Copy(op);
        let m = self.is_marked(p);
        let (mk, join) = (self.new_block(), self.new_block());
        self.branch(m, mk, join);
        self.switch_to(mk);
        self.mark_value(stored, ty);
        self.goto(join);
        self.switch_to(join);
    }

    /// The last element of the array at `arr` (content place) of element type `e`, after a
    /// push.
    pub(super) fn last_elem(&mut self, arr: &Place, e: TyId) -> Place {
        let len = Operand::Copy(super::operand::proj(arr, Proj::Field(1)));
        let k = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, len, cint(1, Ty::U64)));
        self.elem_place(arr, k, e)
    }
}
