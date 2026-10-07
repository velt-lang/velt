//! Drops that can nest without bound (#543). Drop glue that can lead back to itself brackets
//! its work with velt_rt's `drop_enter` / `drop_leave` (`velt_rt/src/drop_depth.rs`): past a
//! fixed depth `enter` says no, the glue `drop_queue`s what it was dropping, and the runtime
//! drops it when the outermost drop is done, so a long chain never overflows the stack.
//!
//! - The object drop of a class whose fields lead back to the class: through arrays, `Map`s,
//!   options, structs or other classes, or through drop glue chosen at run time (interfaces,
//!   classes with subclasses, promises). Self fields that `drop_chain.rs` walks in a loop do
//!   not count: a plain list or tree needs no bracket.
//! - The drop of a struct or enum value that leads back to its own type (through an array).
//!   The value lives in a slot that is freed right after (an array's buffer), so a queued value
//!   is moved to a heap box of its own first (`Glue::QueuedDrop` drops and frees it).
//! - The drop of a closure env whose captures lead to a function value: every chain through
//!   closures passes through an env, so classes and structs need not count function values.

use std::collections::HashSet;

use velt_sema::hir::{TyId, TyKind};

use super::drop_chain::Chain;
use super::Glue;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, unit, Cx, FnLower, Work};
use crate::vir::{self, Operand, Place, Proj, Rvalue, Terminator};

impl Cx<'_> {
    /// Can dropping a class `ty` object nest another drop of a `ty` object (or run glue chosen
    /// at run time), other than through the self fields `chain` loops over?
    pub(super) fn drop_reenters(&mut self, ty: TyId, chain: Option<&Chain>) -> bool {
        let tys = self.adt_field_tys(ty);
        let mut seen = HashSet::new();
        tys.into_iter().enumerate().any(|(i, t)| {
            !chain.is_some_and(|c| c.loops_over(i as u32))
                && self.leads_to(t, Some(ty), false, &mut seen)
        })
    }

    /// Can dropping a struct or enum value of `ty` nest another drop of a `ty` value?
    fn value_drop_reenters(&mut self, ty: TyId) -> bool {
        if self.is_class(ty) || self.boxed(ty) || !matches!(self.kind(ty), TyKind::Adt(..)) {
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
    /// Start a drop (velt_rt `drop_enter`): when the thread is too deep in nested drops,
    /// `queue` hands the value to the runtime and this function returns; else the drop goes
    /// on, and must end with [`Self::leave_drop`].
    fn enter_drop(&mut self, queue: impl FnOnce(&mut Self)) {
        let go = self.rt_u8(Rt::DropEnter, vec![]);
        let (run, queued) = (self.new_block(), self.new_block());
        self.branch(go, run, queued);
        self.switch_to(queued);
        queue(self);
        self.terminate(Terminator::Return(unit()));
        self.switch_to(run);
    }

    /// [`Self::enter_drop`] for the heap object `obj` (a class object or a closure env) that
    /// the drop function `glue` drops: it is queued as it is.
    pub(in crate::lower) fn enter_object_drop(&mut self, obj: Operand, glue: Operand) {
        self.enter_drop(|lw| lw.call_rt(Rt::DropQueue, vec![obj, glue], None));
    }

    /// End a drop [`Self::enter_drop`] started (the outermost one drops the queued values).
    pub(in crate::lower) fn leave_drop(&mut self) {
        self.call_rt(Rt::DropLeave, vec![], None);
    }

    /// The `Glue::Drop` body of `ty` for the value at `*p`, bracketed when it can nest.
    pub(super) fn drop_body(&mut self, p: vir::Local, ty: TyId) {
        let place = self.deref_param(p, ty);
        let bracket = self.cx.value_drop_reenters(ty);
        if bracket {
            let at = place.clone();
            self.enter_drop(|lw| lw.queue_value(at, ty));
        }
        self.drop_expand(&place, ty);
        if bracket {
            self.leave_drop();
        }
        self.terminate(Terminator::Return(unit()));
    }

    /// Move the `ty` value at `place` to a heap box of its own and queue that.
    fn queue_value(&mut self, place: Place, ty: TyId) {
        let vt = self.cx.ty(ty);
        let b = self.alloc(vt);
        let bp = self.operand_place(b.clone(), vir::Ty::Ptr);
        self.assign(
            proj(&bp, Proj::Deref(vt)),
            Rvalue::Use(Operand::Copy(place)),
        );
        let glue = cfunc(self.cx.func(Work::Glue(Glue::QueuedDrop, ty)));
        self.call_rt(Rt::DropQueue, vec![b, glue], None);
    }

    /// `Glue::QueuedDrop`: drop the `ty` value in the heap box `b` and free the box.
    pub(super) fn queued_drop_body(&mut self, b: vir::Local, ty: TyId) {
        let place = self.deref_param(b, ty);
        self.drop_glue(place, ty);
        let vt = self.cx.ty(ty);
        self.free(Operand::Copy(Place::local(b)), vt);
        self.terminate(Terminator::Return(unit()));
    }
}
