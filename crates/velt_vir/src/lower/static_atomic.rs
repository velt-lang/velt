//! Static atomics (docs/internals/design/atomics.md): a module `const` made by
//! `new Atomic<T>(literal)` (sema `static_atomic_init`) has no module state slot. Its word is
//! writable data (`StaticData::mutable`) that starts as the literal, one per program, and the
//! `velt_atomic_*` externs change it in place: no module block, so no module lock.
//!
//! An `Atomic<shared<T> | null>` made with `null` is one too, and its word owns the reference it
//! holds. Module state is dropped at exit for the leak counters, and so are these references:
//! `velt_main` hands the runtime a function releasing them (`velt_rt_statics_drop`,
//! `Work::StaticsDrop`), which it calls then. `velt/sync/epoch`'s registry is one of them.

use velt_sema::hir::{self, ExprKind as K, TyKind};

use super::atomic::is_atomic;
use super::operand::proj;
use super::rt::Rt;
use super::thread_local::literal_word;
use super::{FnLower, ScopeKind, Work};
use crate::vir::{Const, Function, Operand, Place, Proj, Rvalue, StaticData, StaticId, Ty};

impl FnLower<'_, '_> {
    /// The place of global `d`'s `Atomic` when `d` is a static atomic.
    pub(super) fn static_atomic(&mut self, d: hir::DefId) -> Option<Place> {
        let id = self.static_atomic_data(d)?;
        let hir::Def::Global(g) = self.cx.hir.def(d) else {
            return None;
        };
        let vt = self.cx.ty(g.ty);
        let addr = Operand::Const(Const::Static(id), Ty::Ptr);
        Some(proj(&self.operand_place(addr, Ty::Ptr), Proj::Deref(vt)))
    }

    /// Is global `d` a static atomic?
    pub(super) fn is_static_atomic(&mut self, d: hir::DefId) -> bool {
        self.static_atomic_data(d).is_some()
    }

    /// The data holding static atomic `d`'s word, made on first use; `None` for any other
    /// global.
    fn static_atomic_data(&mut self, d: hir::DefId) -> Option<StaticId> {
        if let Some(known) = self.cx.static_atomics.get(&d) {
            return *known;
        }
        let found = match self.cx.hir.def(d) {
            hir::Def::Global(g) if g.slot.is_none() => match &g.init.kind {
                // A computed starting value in std, whose constants are not kept in the module
                // state, stays evaluated at each use (sema `static_atomic_init`).
                K::AdtLit { def, fields, .. }
                    if is_atomic(self.cx, *def) && fields.len() == 1 && literal(&fields[0]) =>
                {
                    Some((g.name.clone(), g.ty, fields[0].clone()))
                }
                _ => None,
            },
            _ => None,
        };
        let id = found.map(|(name, ty, init)| {
            let vt = match self.cx.kind(ty) {
                TyKind::Adt(_, targs) if targs.len() == 1 => self.cx.ty(targs[0]),
                k => super::ice(format_args!("static atomic of type {k:?}")),
            };
            let bits = match vt {
                Ty::Bool => 8,
                t => t.scalar_size().unwrap_or(8) * 8,
            };
            let w = literal_word(&init, vt, bits);
            let bytes = w.to_le_bytes()[..(bits / 8) as usize].to_vec();
            let id = StaticId(self.cx.statics.len() as u32);
            self.cx.statics.push(StaticData {
                align: bits / 8,
                bytes,
                relocs: vec![],
                // The type is part of it: an edit changing it gets a new word (`velt dev`).
                mutable: Some(crate::tls::stable_id(&format!("{name}:{vt:?}"))),
            });
            id
        });
        self.cx.static_atomics.insert(d, id);
        id
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// Before `velt_main` returns: hands the runtime the function releasing the references
    /// static atomics hold, when the program has any.
    pub(super) fn register_statics_drop(&mut self) {
        if self.cx.reference_statics().is_empty() {
            return;
        }
        let f = self.cx.func(Work::StaticsDrop);
        self.call_rt(Rt::StaticsDrop, vec![super::cfunc(f)], None);
    }

    /// `()`: releases the reference each static atomic holds, and empties its word.
    pub(super) fn build_statics_drop(cx: &'c mut super::Cx<'h>) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        lw.push_scope(ScopeKind::Block);
        for (d, t) in lw.cx.reference_statics() {
            let Some(atomic) = lw.static_atomic(d) else {
                continue;
            };
            // The word is the `Atomic`'s one field.
            let addr = lw.addr(atomic);
            let word = proj(&lw.operand_place(addr, Ty::Ptr), Proj::Deref(Ty::Ptr));
            let old = lw.copy_to_temp(Operand::Copy(word.clone()), Ty::Ptr);
            let null = Operand::Const(Const::Int(0), Ty::Ptr);
            lw.assign(word, Rvalue::Use(null));
            lw.drop_glue(Place::local(old), t);
        }
        lw.terminate(crate::vir::Terminator::Return(super::unit()));
        lw.scopes.clear();
        lw.finish("_Gstatics_drop".into(), vec![], Ty::Unit)
    }
}

impl super::Cx<'_> {
    /// The static atomics of the checked program that hold a reference (an
    /// `Atomic<shared<T> | null>` made with `null`), in definition order, with their value type.
    /// Read from the program, not from the static data made so far: a function lowered after
    /// `velt_main` may be the first to use one.
    fn reference_statics(&mut self) -> Vec<(hir::DefId, hir::TyId)> {
        let mut out = vec![];
        let hir = self.hir;
        for (i, def) in hir.defs.iter().enumerate() {
            let hir::Def::Global(g) = def else {
                continue;
            };
            let static_atomic = g.slot.is_none()
                && matches!(&g.init.kind, K::AdtLit { def, fields, .. }
                    if is_atomic(self, *def) && fields.len() == 1 && literal(&fields[0]));
            if !static_atomic {
                continue;
            }
            let TyKind::Adt(_, targs) = self.kind(g.ty) else {
                continue;
            };
            let Some(&t) = targs.first() else {
                continue;
            };
            let reference = match self.kind(t) {
                TyKind::Shared(_) => true,
                TyKind::Option(inner, _) => matches!(self.kind(inner), TyKind::Shared(_)),
                _ => false,
            };
            if reference {
                out.push((hir::DefId(i as u32), t));
            }
        }
        out
    }
}

/// A literal, or a negated number literal (sema's `is_literal`).
fn literal(e: &hir::Expr) -> bool {
    match &e.kind {
        K::Lit(_) => true,
        K::Unary {
            op: hir::UnOp::Neg,
            expr,
        } => matches!(expr.kind, K::Lit(_)),
        _ => false,
    }
}
