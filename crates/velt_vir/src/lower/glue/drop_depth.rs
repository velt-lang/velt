//! Drops that can nest without bound (#543). A drop that can lead back to itself is bracketed:
//! it counts itself inline in the thread's drop state word (velt_rt `drop_state`, see
//! `velt_rt/src/drop_depth.rs`), and past [`MAX_DEPTH`] nested drops it hands the value to
//! velt_rt's queue instead; the outermost drop drains the queue when it ends, so a long chain
//! never overflows the stack. The common case costs one call for the word's address and a few
//! instructions.
//!
//! - In the object drop of a class, each field that leads back to the class: through arrays,
//!   `Map`s, options, structs or other classes, or through drop glue chosen at run time
//!   (interfaces, classes with subclasses, promises). An empty array or a null field skips the
//!   bracket (the leaves of a tree pay nothing), and a queued field is moved to a heap box.
//!   Self fields that `drop_chain.rs` walks in a loop do not count: a plain list or tree needs
//!   no bracket.
//! - The drop of a struct or enum value that leads back to its own type (through an array).
//!   The value lives in a slot that is freed right after (an array's buffer), so a queued value
//!   is moved to a heap box of its own first (`Glue::QueuedDrop` drops and frees it).
//! - The drop of a boxed object type that leads back to itself (`interface Node { next?: Node
//!   }`): the bracket is in the branch that releases the last reference, and the box itself is
//!   queued, still holding that reference (`Glue::QueuedDrop` releases it again).
//! - The drop of a closure env whose captures lead to a function value: every chain through
//!   closures passes through an env, so classes and structs need not count function values.

use std::collections::HashSet;

use velt_sema::hir::{TyId, TyKind};

use super::drop_chain::Chain;
use super::Glue;
use crate::lower::cint;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, unit, Cx, FnLower, Work};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// Nested bracketed drops before the next one is queued (velt_rt's `MAX_DEPTH`; small in unit
/// tests, so that the interpreter's short chains already queue).
const MAX_DEPTH: i128 = if cfg!(test) { 4 } else { 128 };
/// The state word's bit for "something was queued" (velt_rt's `QUEUED`).
const QUEUED: i128 = 1 << 30;
/// The state word's depth bits (velt_rt's `DEPTH`).
const DEPTH: i128 = QUEUED - 1;

impl Cx<'_> {
    /// Can dropping field `f` (of type `t`) of a class `ty` object nest another drop of a `ty`
    /// object (or run glue chosen at run time), other than through the self fields `chain`
    /// loops over?
    pub(super) fn field_drop_reenters(
        &mut self,
        ty: TyId,
        f: u32,
        t: TyId,
        chain: Option<&Chain>,
    ) -> bool {
        !chain.is_some_and(|c| c.loops_over(f))
            && self.leads_to(t, Some(ty), false, &mut HashSet::new())
    }

    /// Can dropping a struct, enum or object type value of `ty` nest another drop of a `ty`
    /// value?
    fn value_drop_reenters(&mut self, ty: TyId) -> bool {
        if self.is_class(ty) || !matches!(self.kind(ty), TyKind::Adt(..)) {
            return false;
        }
        let parts = self.part_types(ty);
        let mut seen = HashSet::new();
        parts
            .into_iter()
            .any(|p| self.leads_to(p, Some(ty), false, &mut seen))
    }

    /// Can dropping a value of `t` run drop glue chosen at run time, a function value's
    /// included?
    pub(in crate::lower) fn drop_runs_unknown(&mut self, t: TyId) -> bool {
        self.leads_to(t, None, true, &mut HashSet::new())
    }

    /// Does the drop of `t` reach a value of type `target`, or drop glue chosen at run time
    /// (with `fns`, a function value's as well)?
    fn leads_to(
        &mut self,
        t: TyId,
        target: Option<TyId>,
        fns: bool,
        seen: &mut HashSet<TyId>,
    ) -> bool {
        if Some(t) == target {
            return true;
        }
        if !self.needs_drop(t) || !seen.insert(t) {
            return false;
        }
        match self.kind(t) {
            TyKind::Array(e) | TyKind::Option(e) | TyKind::Shared(e) => {
                self.leads_to(e, target, fns, seen)
            }
            TyKind::Map(k, v) => {
                self.leads_to(k, target, fns, seen) || self.leads_to(v, target, fns, seen)
            }
            TyKind::FnPtr { .. } | TyKind::Closure(_) => fns,
            TyKind::Dyn(..) | TyKind::Promise(..) => true,
            // A class with subclasses drops through its vtable: any subclass's fields.
            TyKind::Adt(d, _) if self.is_class(t) && self.has_header(d) => true,
            TyKind::Adt(..) if self.is_class(t) => {
                let tys = self.adt_field_tys(t);
                tys.into_iter().any(|f| self.leads_to(f, target, fns, seen))
            }
            TyKind::Adt(..) | TyKind::Tuple(_) | TyKind::Result(..) => {
                let parts = self.part_types(t);
                parts
                    .into_iter()
                    .any(|p| self.leads_to(p, target, fns, seen))
            }
            _ => false,
        }
    }
}

impl FnLower<'_, '_> {
    /// A bracketed drop (module docs): `body` drops the value, unless the thread is
    /// [`MAX_DEPTH`] drops deep, in which case `queue` hands it to the runtime instead. Either
    /// way, code continues after it.
    fn bracket(&mut self, queue: impl FnOnce(&mut Self), body: impl FnOnce(&mut Self)) {
        let state = Place::local(self.temp(Ty::Ptr));
        self.call_rt(Rt::DropState, vec![], Some(state.clone()));
        let sp = self.operand_place(Operand::Copy(state), Ty::Ptr);
        let word = proj(&sp, Proj::Deref(Ty::U32));
        let n = self.rvalue_temp(Ty::U32, Rvalue::Use(Operand::Copy(word.clone())));
        let depth = self.rvalue_temp(
            Ty::U32,
            Rvalue::Binary(BinOp::BitAnd, n.clone(), cint(DEPTH, Ty::U32)),
        );
        let deep = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ge, depth, cint(MAX_DEPTH, Ty::U32)),
        );
        let (queued, run, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(deep, queued, run);
        self.switch_to(queued);
        queue(self);
        self.goto(join);
        self.switch_to(run);
        let up = self.rvalue_temp(Ty::U32, Rvalue::Binary(BinOp::Add, n, cint(1, Ty::U32)));
        self.assign(word.clone(), Rvalue::Use(up));
        body(self);
        // Nested drops leave the depth as they found it, but may have set the queued bit.
        let m = self.rvalue_temp(
            Ty::U32,
            Rvalue::Binary(BinOp::Sub, Operand::Copy(word.clone()), cint(1, Ty::U32)),
        );
        self.assign(word, Rvalue::Use(m.clone()));
        let drain = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, m, cint(QUEUED, Ty::U32)),
        );
        self.when(drain, join);
        self.call_rt(Rt::DropDrain, vec![], None);
        self.goto(join);
        self.switch_to(join);
    }

    /// A bracketed drop of the heap object `obj` (a closure env with its last reference), which
    /// the drop function `glue` drops if it is queued (as it is).
    pub(in crate::lower) fn bracket_object(
        &mut self,
        obj: Operand,
        glue: Operand,
        body: impl FnOnce(&mut Self),
    ) {
        self.bracket(|lw| lw.call_rt(Rt::DropQueue, vec![obj, glue], None), body);
    }

    /// Drop field `fp` (of type `t`) of an object being dropped, bracketed: an empty array or
    /// a null pointer drops at once, anything else may be queued (moved to a heap box).
    pub(super) fn drop_field_bracketed(&mut self, fp: Place, t: TyId) {
        let join = self.new_block();
        if self.cx.ty(t) == Ty::Ptr {
            let v = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(fp.clone())));
            let nn = self.non_null(v);
            self.when(nn, join);
        } else if matches!(self.cx.kind(t), TyKind::Array(_)) {
            let len = Operand::Copy(proj(&fp, Proj::Field(1)));
            let some = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, len, cint(0, Ty::U64)));
            let (empty, full) = (self.new_block(), self.new_block());
            self.branch(some, full, empty);
            self.switch_to(empty);
            self.drop_glue(fp.clone(), t);
            self.goto(join);
            self.switch_to(full);
        }
        let at = fp.clone();
        self.bracket(|lw| lw.queue_value(at, t), |lw| lw.drop_glue(fp, t));
        self.goto(join);
        self.switch_to(join);
    }

    /// The `Glue::Drop` body of `ty` for the value at `*p`, bracketed when it can nest.
    pub(super) fn drop_body(&mut self, p: vir::Local, ty: TyId) {
        let place = self.deref_param(p, ty);
        if !self.cx.value_drop_reenters(ty) {
            self.drop_expand(&place, ty);
        } else if self.cx.boxed(ty) {
            self.drop_boxed_bracketed(&place, ty);
        } else {
            let at = place.clone();
            self.bracket(|lw| lw.queue_value(at, ty), |lw| lw.drop_expand(&place, ty));
        }
        self.terminate(Terminator::Return(unit()));
    }

    /// The drop of the boxed `ty` value at `place` (the box pointer), bracketed in the branch
    /// that releases the last reference: a queued box keeps that reference
    /// (`Glue::QueuedDrop` releases it again).
    fn drop_boxed_bracketed(&mut self, place: &Place, ty: TyId) {
        let payload = self.cx.payload_ty(ty);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let done = self.new_block();
        let nn = self.non_null(p.clone());
        self.when(nn, done);
        let pp = self.operand_place(p.clone(), Ty::Ptr);
        let value = proj(&pp, Proj::Deref(payload));
        let glue = cfunc(self.cx.func(Work::Glue(Glue::QueuedDrop, ty)));
        let q = p.clone();
        self.release(p, |lw| {
            let boxed = q.clone();
            lw.bracket(
                |lw| lw.call_rt(Rt::DropQueue, vec![boxed, glue], None),
                |lw| {
                    lw.drop_inline(&value, ty);
                    lw.counted_free(q, payload);
                },
            );
        });
        self.goto(done);
        self.switch_to(done);
    }

    /// Queue the `ty` value at `place`: a boxed value is its box pointer (its reference moves
    /// to the queue); any other value moves to a heap box of its own.
    fn queue_value(&mut self, place: Place, ty: TyId) {
        let glue = cfunc(self.cx.func(Work::Glue(Glue::QueuedDrop, ty)));
        if self.cx.boxed(ty) {
            let b = Operand::Copy(place);
            return self.call_rt(Rt::DropQueue, vec![b, glue], None);
        }
        let vt = self.cx.ty(ty);
        let b = self.alloc(vt);
        let bp = self.operand_place(b.clone(), Ty::Ptr);
        self.assign(
            proj(&bp, Proj::Deref(vt)),
            Rvalue::Use(Operand::Copy(place)),
        );
        self.call_rt(Rt::DropQueue, vec![b, glue], None);
    }

    /// `Glue::QueuedDrop`: drop the `ty` value in the heap box `b` and free the box; for a
    /// boxed type, `b` is the value itself (a box with its last reference).
    pub(super) fn queued_drop_body(&mut self, b: vir::Local, ty: TyId) {
        if self.cx.boxed(ty) {
            self.drop_glue(Place::local(b), ty);
            self.terminate(Terminator::Return(unit()));
            return;
        }
        let place = self.deref_param(b, ty);
        self.drop_glue(place, ty);
        let vt = self.cx.ty(ty);
        self.free(Operand::Copy(Place::local(b)), vt);
        self.terminate(Terminator::Return(unit()));
    }
}
