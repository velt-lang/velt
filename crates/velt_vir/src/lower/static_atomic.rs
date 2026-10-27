//! Static atomics (docs/internals/design/atomics.md): a module `const` made by
//! `new Atomic<T>(literal)` (sema `static_atomic_init`) has no module state slot. Its word is
//! writable data (`StaticData::mutable`) that starts as the literal, one per program, and the
//! `velt_atomic_*` externs change it in place: no module block, so no module lock.

use velt_sema::hir::{self, ExprKind as K, TyKind};

use super::atomic::is_atomic;
use super::operand::proj;
use super::thread_local::literal_word;
use super::FnLower;
use crate::vir::{Const, Operand, Place, Proj, StaticData, StaticId, Ty};

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
                K::AdtLit { def, fields, .. } if is_atomic(self.cx, *def) && fields.len() == 1 => {
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
                mutable: Some(crate::tls::stable_id(&name)),
            });
            id
        });
        self.cx.static_atomics.insert(d, id);
        id
    }
}
