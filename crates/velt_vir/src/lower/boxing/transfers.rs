//! Counted types a transfer can copy: the clone glue of these looks up copies made earlier in
//! the same transfer (velt_rt `transfer_map`, glue/transfer.rs `find_copy`), so a graph keeps its
//! sharing and cycles on the other thread. Other types never pay for the lookup: a program that
//! hands nothing to another thread clones exactly as before.
//!
//! What a function value, an interface value or a class with subclasses holds is not in its
//! static type, so the walk goes on through what this pass actually built: the capture types of
//! every closure environment, the concrete types put into interface values, and the concrete
//! classes of each hierarchy (`Facts`).

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

    /// Record the (concrete) type of a closure environment's capture.
    pub(in crate::lower) fn note_capture(&mut self, t: TyId) {
        self.facts.captures.insert(t);
    }

    /// Record a concrete type that an interface value or a class reference may hold (a vtable
    /// was built for it).
    pub(in crate::lower) fn note_vtable_type(&mut self, t: TyId) {
        self.facts.vtable_types.insert(t);
    }

    /// May a transfer deep-copy a counted `t` (so its copies are looked up by identity)?
    pub(in crate::lower) fn copied_across(&self, t: TyId) -> bool {
        self.boxing.transferred.contains(&t)
    }

    /// Fill `next.transferred` with the counted types (as `next` counts them) reachable from the
    /// transferred types of this pass.
    pub(super) fn close_transfers(&mut self, next: &mut Boxing) {
        let mut seen = HashSet::new();
        let mut work: Vec<TyId> = self.facts.transfers.iter().copied().collect();
        let (mut captures_done, mut impls_done) = (false, false);
        while let Some(t) = work.pop() {
            if !seen.insert(t) {
                continue;
            }
            if self.counted_in(next, t) {
                next.transferred.insert(t);
            }
            match self.kind(t) {
                // A function value may be any closure of the program.
                TyKind::Closure(_) | TyKind::FnPtr { .. } => {
                    if !std::mem::replace(&mut captures_done, true) {
                        work.extend(self.facts.captures.iter().copied());
                    }
                }
                // An interface value may hold any type an interface table was built for.
                TyKind::Dyn(..) => {
                    if !std::mem::replace(&mut impls_done, true) {
                        work.extend(self.facts.vtable_types.iter().copied());
                    }
                }
                // A `shared<T>` is not copied; a promise's result is transferred on its own.
                TyKind::Shared(_) | TyKind::Promise(..) => {}
                TyKind::Array(e) | TyKind::Option(e) => work.push(e),
                TyKind::Adt(d, _) if self.is_class(t) => {
                    work.extend(self.adt_field_tys(t));
                    if self.has_header(d) {
                        // It may hold any class of its hierarchy that was built.
                        let root = self.class_root(d);
                        let classes: Vec<TyId> = self.facts.vtable_types.iter().copied().collect();
                        work.extend(classes.into_iter().filter(|&c| {
                            matches!(self.kind(c), TyKind::Adt(cd, _)
                                if self.is_class(c) && self.class_root(cd) == root)
                        }));
                    }
                }
                _ => work.extend(self.part_types(t)),
            }
        }
    }
}
