//! Counted types a transfer can copy: the clone glue of these looks up copies made earlier in
//! the same transfer (velt_rt `transfer_map`, glue/transfer.rs `find_copy`), so a graph keeps its
//! sharing and cycles on the other thread. Other types never pay for the lookup: a program that
//! hands nothing to another thread clones exactly as before.

use std::collections::HashSet;

use velt_sema::hir::{TyId, TyKind};

use super::Boxing;
use crate::lower::Cx;

impl Cx<'_> {
    /// Record that values of `t` are transferred as a whole (a `TransferRoot` glue, a copy for
    /// another thread).
    pub(in crate::lower) fn note_transfer(&mut self, t: TyId) {
        self.facts.transfers.insert(t);
    }

    /// May a transfer deep-copy a counted `t` (so its copies are looked up by identity)?
    pub(in crate::lower) fn copied_across(&self, t: TyId) -> bool {
        self.boxing.transfer_any || self.boxing.transferred.contains(&t)
    }

    /// Fill `next.transferred` with the counted types (as `next` counts them) reachable from the
    /// transferred types of this pass.
    pub(super) fn close_transfers(&mut self, next: &mut Boxing) {
        let mut seen = HashSet::new();
        let mut work: Vec<TyId> = self.facts.transfers.iter().copied().collect();
        while let Some(t) = work.pop() {
            if !seen.insert(t) {
                continue;
            }
            if self.counted_in(next, t) {
                next.transferred.insert(t);
            }
            match self.kind(t) {
                // What a function or interface value reaches is not known here.
                TyKind::Dyn(..) | TyKind::Closure(_) | TyKind::FnPtr { .. } => {
                    next.transfer_any = true;
                    return;
                }
                // A `shared<T>` is not copied; a promise's result is transferred on its own.
                TyKind::Shared(_) | TyKind::Promise(..) => {}
                TyKind::Array(e) | TyKind::Option(e) => work.push(e),
                // A class in a hierarchy may hold a subclass, whose fields are not known here.
                TyKind::Adt(d, _) if self.is_class(t) && self.has_header(d) => {
                    next.transfer_any = true;
                    return;
                }
                TyKind::Adt(..) if self.is_class(t) => work.extend(self.adt_field_tys(t)),
                _ => work.extend(self.part_types(t)),
            }
        }
    }
}
