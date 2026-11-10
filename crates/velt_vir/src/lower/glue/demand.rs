//! Interface tables' glue entries on demand.
//!
//! Every interface table (`VtableKey::Impl`) has clone, format, share and transfer entries
//! (`SLOT_CLONE`, `SLOT_FORMAT`, `SLOT_SHARE`, `SLOT_TRANSFER`), and an entry makes lowering
//! build its glue and everything that glue reaches. Most programs never clone, log, share or send
//! most of their interface values, and that glue was about a fifth of the code of a generated
//! application.
//!
//! The only readers of these entries are the dispatches on an interface value of a known
//! interface type, all of them in glue bodies: `clone_expand` (glue/clone.rs), `format_value`'s
//! interface arm (glue/format.rs), `many_check_expand` (glue/many.rs), `transfer_expand`
//! (glue/transfer.rs) and `share_into` (share.rs). Each notes the slot and the interface while
//! the function holding it is built ([`FnLower::note_dyn_glue`]). A table built before its
//! interface's slot is dispatched leaves the entry out and is remembered
//! ([`Cx::defer_iface_glue`]); the first dispatch fills the entries of every table of that
//! interface built so far, which queues their glue ([`Cx::demand_iface_glue`]), and tables built
//! after it get the entry at once. The glue itself dispatches on the interface values it holds,
//! so what it reaches is filled the same way while the worklist drains.
//!
//! An entry nothing dispatches stays null. The interface is the key, not its type arguments: a
//! dispatch on an `I<A>` fills the tables of every `I<B>`, which is more than needed but never
//! less (an adapter to `I<B>` has tables of its own, and its glue dispatches on the interface
//! value it wraps). Left as they were:
//! - `any` tables (the implementation of the `any` interface): the runtime reads their share,
//!   transfer and drop entries (`velt_rt::fn_props`), and `any` values reach every operation
//!   through dynamic code;
//! - drop entries, which every interface value needs;
//! - generic closures' tables (`VtableKey::Closure`), few and small;
//! - every table of a `velt dev` version (`GlueSlots::keep_all`): a value made by one version
//!   keeps that version's table, and a later version's code may clone, log or send it.
//!
//! `VELT_GLUE_DEMAND=off` keeps every entry, to compare builds.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_sema::hir::{DefId, TyId, TyKind};

use super::vtable::HIDDEN;
use super::{Glue, SLOT_CLONE, SLOT_FORMAT, SLOT_SHARE, SLOT_TRANSFER};
use crate::lower::{ice, Cx, FnLower, Work};
use crate::vir::{Const, StaticId};

/// The interface table entries made on demand, with their glue.
pub(super) const ON_DEMAND: [(i128, Glue); 4] = [
    (SLOT_CLONE, Glue::DynClone),
    (SLOT_FORMAT, Glue::DynFormat),
    (SLOT_SHARE, Glue::DynShare),
    (SLOT_TRANSFER, Glue::DynTransfer),
];

/// What the program dispatches on interface values, and the entries still left out.
#[derive(Debug, Default)]
pub(in crate::lower) struct IfaceDemand {
    /// `(slot, interface)` pairs some function built so far dispatches.
    demanded: FxHashSet<(i128, DefId)>,
    /// Per `(slot, interface)` not demanded yet: the tables built without that entry, with
    /// the concrete type each is for.
    pending: FxHashMap<(i128, DefId), Vec<(StaticId, TyId)>>,
}

/// Whether `VELT_GLUE_DEMAND=off` asks for every entry.
pub(in crate::lower) fn demand_off() -> bool {
    std::env::var_os("VELT_GLUE_DEMAND").is_some_and(|v| v == "off")
}

impl Cx<'_> {
    /// Whether the table of implementation `index` gets its `slot` entry (one of [`ON_DEMAND`])
    /// when it is built.
    pub(super) fn iface_glue_now(&self, index: u32, slot: i128) -> bool {
        let iface = self.hir.impls[index as usize].iface;
        self.glue_slots.keep_all
            || self.is_any_impl(index)
            || self.glue_slots.iface.demanded.contains(&(slot, iface))
    }

    /// Remember the entries table `id` of implementation `index` for `ty` was built without, to
    /// fill them once something dispatches them (module docs).
    pub(super) fn defer_iface_glue(&mut self, id: StaticId, index: u32, ty: TyId) {
        let iface = self.hir.impls[index as usize].iface;
        for (slot, _) in ON_DEMAND {
            if !self.iface_glue_now(index, slot) {
                let pending = &mut self.glue_slots.iface.pending;
                pending.entry((slot, iface)).or_default().push((id, ty));
            }
        }
    }

    /// Something dispatches `slot` on values of interface `iface`: the tables of that interface
    /// built so far get the entry, and so does every one built from now on (module docs).
    pub(in crate::lower) fn demand_iface_glue(&mut self, slot: i128, iface: DefId) {
        if !self.glue_slots.iface.demanded.insert((slot, iface)) {
            return;
        }
        let Some(tables) = self.glue_slots.iface.pending.remove(&(slot, iface)) else {
            return;
        };
        let Some(&(_, glue)) = ON_DEMAND.iter().find(|e| e.0 == slot) else {
            ice("an interface glue slot that is not made on demand")
        };
        let at = (8 * (slot + HIDDEN)) as u32;
        for (id, ty) in tables {
            let f = self.func(Work::Glue(glue, ty));
            self.statics[id.0 as usize]
                .relocs
                .push((at, Const::Func(f)));
        }
    }
}

impl FnLower<'_, '_> {
    /// The function being built dispatches `slot` (one of [`ON_DEMAND`]) on a value of type `ty`
    /// through its table: an interface type's tables need that entry (module docs).
    pub(in crate::lower) fn note_dyn_glue(&mut self, slot: i128, ty: TyId) {
        if let TyKind::Dyn(iface, _) = self.cx.kind(ty) {
            self.cx.demand_iface_glue(slot, iface);
        }
    }
}
