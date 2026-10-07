//! Drop glue of self-referential classes without recursion (#543): a linked list
//! (`class N { next: N | null }`) or a tree dropped by plain recursion needs one stack frame per
//! node on the path, so a long chain overflowed the stack (wasm at a few thousand nodes, native
//! at a few hundred thousand). The object drop of such a class loops instead:
//!
//! - **Tail loop.** The last field of the class's own type (`T` or `T | null`) is the tail: the
//!   loop drops a node's other fields, frees it and continues with the tail's node, which it now
//!   owns (for a counted class: the count was 1; a shared node is only decremented and ends the
//!   loop). This is the recursive drop with its last call made a jump, so it is exact when no
//!   field after the tail has an observable drop (a `[Symbol.dispose]()` somewhere in it); the
//!   class itself may have one, which runs as each node is taken, as before.
//! - **Rotation.** A node with several such fields (a binary tree) would still recurse into all
//!   but the last. When nothing in the class's drop is observable (no dispose hook in the class
//!   or its other fields), the loop first moves each other self field's node onto the chain:
//!   with `c = cur.left` owned, `cur.left = c.right; c.right = cur; cur = c`, the classic
//!   constant-space tree destruction. Every node is visited a bounded number of times, so it
//!   is linear, and no shape of tree needs stack.
//!
//! Classes in a hierarchy with a vtable keep the recursive drop (a field of the base type may
//! hold a subclass, whose drop goes through its vtable); so do self references through arrays,
//! maps and other types.

use std::collections::HashSet;

use velt_sema::hir::{TyId, TyKind};

use crate::lower::{cint, unit, Cx, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Rvalue, Terminator, Ty};

/// How the object drop of a self-referential class walks its nodes.
pub(super) struct Chain {
    /// The field whose node the loop continues with (the last field of the class's own type).
    tail: u32,
    /// The other fields of the class's own type, rotated onto the chain (empty unless nothing in
    /// the drop is observable: they then drop recursively, in their place).
    rotate: Vec<u32>,
}

impl Cx<'_> {
    /// Does a field of type `t` hold an object of the class `ty` (`ty` or `ty | null`)?
    fn holds_self(&mut self, t: TyId, ty: TyId) -> bool {
        t == ty || matches!(self.kind(t), TyKind::Option(e) if e == ty)
    }

    /// Is dropping a value of `t` unobservable: can it never run a `[Symbol.dispose]()` hook
    /// (only memory is freed, in whatever order)? Conservative: function values, interfaces,
    /// promises and `shared` values may hold anything.
    pub(super) fn drop_is_silent(&mut self, t: TyId) -> bool {
        self.silent_in(t, &mut HashSet::new())
    }

    fn silent_in(&mut self, t: TyId, seen: &mut HashSet<TyId>) -> bool {
        if !self.needs_drop(t) || !seen.insert(t) {
            // A type reached again through itself is decided where it was first reached.
            return true;
        }
        match self.kind(t) {
            TyKind::Str => true,
            TyKind::Array(e) | TyKind::Option(e) => self.silent_in(e, seen),
            TyKind::Map(k, v) => self.silent_in(k, seen) && self.silent_in(v, seen),
            TyKind::Adt(d, _) => {
                if self.dispose_of(d).is_some() || (self.is_class(t) && self.has_header(d)) {
                    return false;
                }
                let parts = self.part_types(t);
                parts.into_iter().all(|p| self.silent_in(p, seen))
            }
            TyKind::Tuple(_) | TyKind::Result(..) => {
                let parts = self.part_types(t);
                parts.into_iter().all(|p| self.silent_in(p, seen))
            }
            _ => false,
        }
    }

    /// The loop plan of the object drop of class `ty`, or `None` to drop recursively.
    pub(super) fn drop_chain(&mut self, ty: TyId) -> Option<Chain> {
        let TyKind::Adt(d, _) = self.kind(ty) else {
            return None;
        };
        if !self.is_class(ty) || self.has_header(d) || self.is_generator_obj(ty) {
            return None;
        }
        let tys = self.adt_field_tys(ty);
        let mut own = vec![];
        for (i, &t) in tys.iter().enumerate() {
            if self.holds_self(t, ty) {
                own.push(i as u32);
            }
        }
        let tail = own.pop()?;
        let after = tys[tail as usize + 1..].to_vec();
        if !after.into_iter().all(|t| self.drop_is_silent(t)) {
            return None;
        }
        let others: Vec<TyId> = tys
            .iter()
            .enumerate()
            .filter(|(i, _)| !own.contains(&(*i as u32)) && *i as u32 != tail)
            .map(|(_, &t)| t)
            .collect();
        let silent = self.dispose_of(d).is_none()
            && others.into_iter().all(|t| self.drop_is_silent(t));
        Some(Chain {
            tail,
            rotate: if silent { own } else { vec![] },
        })
    }
}

impl FnLower<'_, '_> {
    /// The object drop of class `ty` as a loop over its nodes (module docs), from `obj`.
    pub(super) fn obj_drop_chain_body(&mut self, obj: vir::Local, ty: TyId, chain: Chain) {
        let cur = Place::local(self.temp(Ty::Ptr));
        self.assign(cur.clone(), Rvalue::Use(Operand::Copy(Place::local(obj))));
        // Every node is disposed when it is taken, before it enters the loop.
        self.call_dispose(Operand::Copy(cur.clone()), ty);
        let top = self.new_block();
        let done = self.new_block();
        self.goto(top);
        self.switch_to(top);
        for &f in &chain.rotate {
            self.rotate_field(&cur, ty, f, chain.tail, top);
        }
        let tys = self.cx.adt_field_tys(ty);
        for (i, t) in tys.into_iter().enumerate() {
            let i = i as u32;
            if i != chain.tail && !chain.rotate.contains(&i) && self.cx.needs_drop(t) {
                let fp = self.field_place(&cur, ty, i);
                self.drop_glue(fp, t);
            }
        }
        let tail = self.field_place(&cur, ty, chain.tail);
        let next = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(tail)));
        self.object_free(Operand::Copy(cur.clone()), ty);
        let nn = self.non_null(next.clone());
        self.when(nn, done);
        self.take(next.clone(), ty, done);
        self.assign(cur.clone(), Rvalue::Use(next));
        self.call_dispose(Operand::Copy(cur.clone()), ty);
        self.goto(top);
        self.switch_to(done);
        self.terminate(Terminator::Return(unit()));
    }

    /// One rotation step for self field `f` of the node at `cur`: when it holds a node this drop
    /// owns, that node takes `cur`'s place (`cur.f = c.tail; c.tail = cur; cur = c`) and the
    /// loop restarts at `top`; a node shared elsewhere is only released (and the field
    /// cleared); either way an empty field falls through to the next step.
    fn rotate_field(&mut self, cur: &Place, ty: TyId, f: u32, tail: u32, top: vir::BlockId) {
        let fp = self.field_place(cur, ty, f);
        let c = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(fp.clone())));
        let next = self.new_block();
        let nn = self.non_null(c.clone());
        self.when(nn, next);
        let shared = self.new_block();
        self.take(c.clone(), ty, shared);
        let cp = self.operand_place(c.clone(), Ty::Ptr);
        let ctail = self.field_place(&cp, ty, tail);
        self.assign(fp.clone(), Rvalue::Use(Operand::Copy(ctail.clone())));
        self.assign(ctail, Rvalue::Use(Operand::Copy(cur.clone())));
        self.assign(cur.clone(), Rvalue::Use(c));
        self.goto(top);
        self.switch_to(shared);
        self.assign(fp, Rvalue::Use(cint(0, Ty::Ptr)));
        self.goto(next);
        self.switch_to(next);
    }

    /// Take ownership of the non-null node `p` that a dropped node referred to: continue when
    /// this was its last reference (an uncounted class always is), else release that reference
    /// and go to `shared`.
    fn take(&mut self, p: Operand, ty: TyId, shared: vir::BlockId) {
        if !self.cx.counted(ty) {
            return;
        }
        let c = self.count_place(p);
        let n = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(c.clone())));
        let last = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, n.clone(), cint(1, Ty::U64)),
        );
        let (own, dec) = (self.new_block(), self.new_block());
        self.branch(last, own, dec);
        self.switch_to(dec);
        let m = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, n, cint(1, Ty::U64)));
        self.assign(c, Rvalue::Use(m));
        self.goto(shared);
        self.switch_to(own);
    }
}
