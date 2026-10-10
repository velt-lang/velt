//! Weak-capable types (docs/internals/design/weak-refs.md, "Rules for the compiler"): the types
//! whose objects may be weakly held, so their release takes the three-way sequence that tells a
//! weakly held object apart (rc.rs `release_weak`).
//!
//! They are the counted types of weak keys and `WeakRef` targets, and every counted type
//! reachable from a weak map's key and value types the way the trace glue walks objects
//! (glue/trace.rs): through fields, elements and payloads stored inline, up to the next counted
//! object. A class in a hierarchy with a vtable is weak-capable but traced as opaque (its glue
//! is null), so nothing is reached through it. A program without `WeakMap`, `WeakSet` or
//! `WeakRef` notes no seeds and has no weak-capable types: it compiles exactly as before.

use std::collections::HashSet;

use velt_sema::hir::{TyId, TyKind};

use super::Boxing;
use crate::lower::Cx;

impl Cx<'_> {
    /// Record that values of `t` are weak keys, `WeakRef` targets or weak map values.
    pub(in crate::lower) fn note_weak_seed(&mut self, t: TyId) {
        self.facts.weak_seeds.insert(t);
    }

    /// Does the program use weak references at all (are there weak-capable types)?
    pub(in crate::lower) fn has_weak(&self) -> bool {
        !self.boxing.weak.is_empty() || !self.boxing.weak_roots.is_empty()
    }

    /// Is `t` a weak-capable counted type: does its release go through the three-way sequence?
    pub(in crate::lower) fn weak_capable(&mut self, t: TyId) -> bool {
        if !self.has_weak() {
            return false;
        }
        match self.kind(t) {
            TyKind::Adt(d, _) if self.is_class(t) => {
                let root = self.class_root(d);
                self.boxing.weak_roots.contains(&root)
            }
            _ => self.boxing.weak.contains(&t),
        }
    }

    /// Does the trace glue look into objects of the counted type `t` (not a class with a
    /// vtable, whose dynamic class may have fields its static type lacks)?
    pub(in crate::lower) fn traceable(&mut self, t: TyId) -> bool {
        match self.kind(t) {
            TyKind::Adt(d, _) if self.is_class(t) => !self.has_header(d),
            _ => true,
        }
    }

    /// The counted types an inline value of `t` refers to directly, as the trace glue visits
    /// them (`b`: the counted types; `None`: this pass's).
    pub(in crate::lower) fn weak_refs_inline(&mut self, b: Option<&Boxing>, t: TyId) -> Vec<TyId> {
        let mut out = vec![];
        self.refs_inline(b, t, &mut out, &mut HashSet::new());
        out
    }

    fn refs_inline(
        &mut self,
        b: Option<&Boxing>,
        t: TyId,
        out: &mut Vec<TyId>,
        seen: &mut HashSet<TyId>,
    ) {
        let counted = match b {
            Some(b) => self.counted_in(b, t),
            None => self.counted(t),
        };
        if counted {
            if !out.contains(&t) {
                out.push(t);
            }
            return;
        }
        if !seen.insert(t) {
            return;
        }
        match self.kind(t) {
            TyKind::Option(e) | TyKind::Array(e) => self.refs_inline(b, e, out, seen),
            // An uncounted object is opaque: nothing is traced through it (a leak at worst).
            TyKind::Adt(..) if self.is_class(t) => {}
            TyKind::Adt(..) | TyKind::Tuple(_) | TyKind::Result(..) => {
                for p in self.part_types(t) {
                    self.refs_inline(b, p, out, seen);
                }
            }
            _ => {}
        }
    }

    /// The counted types an object of the counted type `t` refers to directly (what its trace
    /// glue visits); none for an opaque one.
    pub(in crate::lower) fn weak_refs_of(&mut self, b: Option<&Boxing>, t: TyId) -> Vec<TyId> {
        if !self.traceable(t) {
            return vec![];
        }
        let mut out = vec![];
        let mut seen = HashSet::new();
        let parts = match self.kind(t) {
            TyKind::Adt(..) if self.is_class(t) => self.adt_field_tys(t),
            TyKind::Array(e) => vec![e],
            _ => self.part_types(t),
        };
        for p in parts {
            self.refs_inline(b, p, &mut out, &mut seen);
        }
        out
    }

    /// Display names of the weak-capable types (`VELT_DEBUG_COUNTED=1`).
    pub(in crate::lower) fn weak_names(&self) -> Vec<String> {
        let roots = self.boxing.weak_roots.iter().map(|d| match self.hir.def(*d) {
            velt_sema::hir::Def::Adt(a) => format!("class {}", a.name),
            _ => "?".into(),
        });
        let tys = self.boxing.weak.iter().map(|&t| self.type_name(t));
        let mut out: Vec<String> = roots.chain(tys).collect();
        out.sort();
        out
    }

    /// Fill `next`'s weak-capable types from this pass's seeds.
    pub(super) fn close_weak(&mut self, next: &mut Boxing) {
        let seeds: Vec<TyId> = self.facts.weak_seeds.iter().copied().collect();
        let mut work = vec![];
        for s in seeds {
            let refs = self.weak_refs_inline(Some(next), s);
            work.extend(refs);
        }
        // Only the weak sets change below; the counted types are fixed.
        let snapshot = next.clone();
        let mut seen = HashSet::new();
        while let Some(t) = work.pop() {
            if !seen.insert(t) {
                continue;
            }
            match self.kind(t) {
                TyKind::Adt(d, _) if self.is_class(t) => {
                    let root = self.class_root(d);
                    next.weak_roots.insert(root);
                }
                _ => {
                    next.weak.insert(t);
                }
            }
            work.extend(self.weak_refs_of(Some(&snapshot), t));
        }
    }
}
