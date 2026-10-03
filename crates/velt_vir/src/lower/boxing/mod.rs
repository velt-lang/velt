//! Counted types (semantics stage 2, docs/design/semantics-stage2.md §3.1): which concrete types
//! carry a reference count because the program shares their values.
//!
//! Lowering records *facts* while it builds functions — every share of a value of type `T`
//! ([`Cx::note_share`]) and every borrow of a place reached through a value of type `C` that
//! has to stay alive while user code runs ([`Cx::note_projection`], see `stabilize.rs`).
//! [`Cx::close_boxing`] turns them into the next [`Boxing`]; `lower_program` repeats lowering
//! until the set is stable. Programs that share nothing lower once, with today's
//! representation everywhere.

mod kinds;

use std::collections::HashSet;

use velt_sema::hir::{DefId, TyId, TyKind};

use super::Cx;

pub(super) use kinds::ShareKind;

/// The counted types of one lowering pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Boxing {
    /// Class hierarchies (by root class) whose objects carry a count in front of them.
    roots: HashSet<DefId>,
    /// Arrays and object types whose values are counted boxes (`Ptr` to the inline value with
    /// the count in front) instead of inline values.
    boxes: HashSet<TyId>,
    /// Types of values borrowed in place inside a counted object: a pointer to one may have
    /// other owners, so params of these types are never `noalias`.
    interior: HashSet<TyId>,
    /// Interface types whose values are compared by identity: an object type converted to one
    /// is counted, so the interface value points at the object itself (`make_dyn`).
    identity_dyns: HashSet<TyId>,
    /// The program compares function values: every evaluation of a closure without captures
    /// gets an environment of its own as its identity (closure.rs).
    fn_identity: bool,
}

/// What a lowering pass observed (see the module docs).
#[derive(Debug, Default)]
pub(super) struct Facts {
    /// Types of shared values.
    shares: HashSet<TyId>,
    /// `(container, value)`: a borrowed place of type `value` reached through a `container`.
    projections: HashSet<(TyId, TyId)>,
    /// Object types compared by identity (`==`, same.rs).
    identity: HashSet<TyId>,
    /// Function values are compared (or hashed) somewhere.
    fn_compared: bool,
    /// A share was lowered as a placeholder because its type was not counted yet: the output
    /// of this pass must not be used.
    pub(super) unmet: bool,
}

impl Cx<'_> {
    /// Record that a value of type `t` is shared.
    pub(super) fn note_share(&mut self, t: TyId) {
        self.facts.shares.insert(t);
    }

    /// Record that values of the object type `t` are compared by identity.
    pub(super) fn note_identity(&mut self, t: TyId) {
        self.facts.identity.insert(t);
    }

    /// Record that function values are compared by identity.
    pub(super) fn note_fn_identity(&mut self) {
        self.facts.fn_compared = true;
    }

    /// Do closures need an identity of their own (see [`Boxing::fn_identity`])?
    pub(super) fn fn_identity(&self) -> bool {
        self.boxing.fn_identity
    }

    /// The interface value type `dyn_ty` is compared by identity, and its implementor `t` is an
    /// object type that a conversion would copy: record that `t` must be counted.
    pub(super) fn note_dyn_identity(&mut self, dyn_ty: TyId, t: TyId) {
        if self.boxing.identity_dyns.contains(&dyn_ty) && self.copied_object(t) {
            self.facts.identity.insert(t);
            self.facts.shares.insert(t);
        }
    }

    /// Record a borrow of a `value`-typed place reached through a `container`-typed value.
    pub(super) fn note_projection(&mut self, container: TyId, value: TyId) {
        self.facts.projections.insert((container, value));
    }

    /// Does a value of `t` carry a reference count?
    pub(super) fn counted(&self, t: TyId) -> bool {
        self.counted_in(&self.boxing, t)
    }

    /// Are references to values of `t` provably unique while a call borrows them (no other
    /// owner can reach the value: it is neither counted nor borrowed inside a counted object)?
    /// Only then may a param of type `t` be `noalias` (vir.rs invariant 9).
    pub(super) fn unique_refs(&self, t: TyId) -> bool {
        !self.counted(t) && !self.boxing.interior.contains(&t)
    }

    /// Display names of the counted types (`VELT_DEBUG_COUNTED=1`, bench/rc_stats.ps1).
    pub(super) fn counted_names(&self) -> Vec<String> {
        let roots = self.boxing.roots.iter().map(|d| match self.hir.def(*d) {
            velt_sema::hir::Def::Adt(a) => format!("class {}", a.name),
            _ => "?".into(),
        });
        let boxes = self.boxing.boxes.iter().map(|&t| self.type_name(t));
        let mut out: Vec<String> = roots.chain(boxes).collect();
        out.sort();
        out
    }

    /// The counted types implied by this pass's facts (a superset of the current ones).
    pub(super) fn close_boxing(&mut self) -> Boxing {
        let mut next = self.boxing.clone();
        next.fn_identity |= self.facts.fn_compared;
        let dyns = self.facts.identity.iter().copied();
        let dyns: Vec<TyId> = dyns
            .filter(|t| matches!(self.kind(*t), TyKind::Dyn(..)))
            .collect();
        next.identity_dyns.extend(dyns);
        let mut work: Vec<TyId> = self.facts.shares.iter().copied().collect();
        let mut seen = HashSet::new();
        loop {
            while let Some(t) = work.pop() {
                if seen.insert(t) {
                    self.close_share(t, &mut next, &mut work);
                }
            }
            // A borrow through a counted container keeps the borrowed value alive by sharing it.
            let projections: Vec<(TyId, TyId)> = self.facts.projections.iter().copied().collect();
            for (c, v) in projections {
                if self.counted_in(&next, c) {
                    next.interior.insert(v);
                    if !seen.contains(&v) {
                        work.push(v);
                    }
                }
            }
            if work.is_empty() {
                return next;
            }
        }
    }

    /// What sharing a value of `t` requires of `next`.
    fn close_share(&mut self, t: TyId, next: &mut Boxing, work: &mut Vec<TyId>) {
        // An object type copied when shared would lose its identity: compared by identity, a
        // shared one is one counted object.
        if self.facts.identity.contains(&t) && self.copied_object(t) {
            next.boxes.insert(t);
            return;
        }
        match self.share_kind(t) {
            ShareKind::Object => match self.kind(t) {
                TyKind::Adt(d, _) if self.is_class(t) => {
                    next.roots.insert(self.class_root(d));
                }
                _ => {
                    next.boxes.insert(t);
                }
            },
            ShareKind::Value => work.extend(self.part_types(t)),
            _ => {}
        }
    }

    /// `counted` against a candidate set.
    fn counted_in(&self, b: &Boxing, t: TyId) -> bool {
        match self.types.kind(t) {
            TyKind::Adt(d, _) if self.is_class(t) => b.roots.contains(&self.class_root(*d)),
            _ => b.boxes.contains(&t),
        }
    }

    /// Is `t` an array or object type stored as a counted box (a `Ptr` to its inline value)?
    pub(super) fn boxed(&self, t: TyId) -> bool {
        self.boxing.boxes.contains(&t)
    }

    /// The inline layout of a boxed type's value (what the box holds).
    pub(super) fn payload_ty(&mut self, t: TyId) -> crate::vir::Ty {
        match self.kind(t) {
            TyKind::Array(_) => crate::vir::Ty::Agg(self.array_agg()),
            _ => crate::vir::Ty::Agg(self.value_agg(t)),
        }
    }
}
