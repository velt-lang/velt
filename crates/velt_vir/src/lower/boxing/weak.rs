//! Weak-capable types (docs/internals/design/weak-refs.md, "Rules for the compiler"): the types
//! whose objects may be weakly held, so their release takes the three-way sequence that tells a
//! weakly held object apart (rc.rs `release_weak`).
//!
//! They are the counted types of weak keys and `WeakRef` targets, and every counted type
//! reachable from a weak map's key and value types the way the trace glue walks objects
//! (glue/trace.rs): through fields, elements and payloads stored inline, up to the next counted
//! object. A class in a hierarchy with a vtable is traced by its dynamic class
//! (glue/trace_class.rs), so what any of its built classes refers to is reached through it. A program without `WeakMap`, `WeakSet` or
//! `WeakRef` notes no seeds and has no weak-capable types: it compiles exactly as before.

use rustc_hash::FxHashSet;

use velt_sema::hir::{TyId, TyKind};

use super::Boxing;
use crate::lower::Cx;

impl Cx<'_> {
    /// Record that values of `t` are weak keys, `WeakRef` targets or weak map values.
    pub(in crate::lower) fn note_weak_seed(&mut self, t: TyId) {
        self.facts.weak_seeds.insert(t);
    }

    /// Record that `object` is a weak key or a `WeakRef` target (issue #926).
    pub(in crate::lower) fn note_weak_object(&mut self) {
        self.facts.weak_object = true;
    }

    /// Record that dynamic values hold values of the concrete type `t` (an `any` table).
    pub(in crate::lower) fn note_dyn_type(&mut self, t: TyId) {
        self.facts.dyn_types.insert(t);
    }

    /// Record that values of the concrete type `t` convert to `object` (`None`: an `any`,
    /// `unknown` or interface value does, which may hold any type).
    pub(in crate::lower) fn note_to_object(&mut self, t: Option<TyId>) {
        match t {
            Some(t) => {
                self.facts.object_types.insert(t);
            }
            None => self.facts.object_from_dyn = true,
        }
    }

    /// Is `object` a weak key or `WeakRef` target in this program (see `Boxing::weak_object`)?
    pub(in crate::lower) fn weak_object(&self) -> bool {
        self.boxing.weak_object
    }

    /// Can a dynamic value holding a `t` be a weak key: is its data pointer a counted,
    /// weak-capable object (not a box made by the conversion)?
    pub(in crate::lower) fn weak_keyable(&mut self, t: TyId) -> bool {
        (self.is_class(t) || self.boxed(t)) && self.counted(t) && self.weak_capable(t)
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
        let t = self.ph_rep(t);
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
        self.refs_inline(b, t, &mut out, &mut FxHashSet::default());
        out
    }

    fn refs_inline(
        &mut self,
        b: Option<&Boxing>,
        t: TyId,
        out: &mut Vec<TyId>,
        seen: &mut FxHashSet<TyId>,
    ) {
        let counted = match b {
            Some(b) => self.counted_in(b, t),
            None => self.counted(t),
        };
        if counted {
            if !out.contains(&t) {
                out.push(t);
            }
            // A typed proxy (#823) is its `Proxy` object, which the trace glue visits instead.
            let proxy = b.unwrap_or(&self.boxing).proxy_class;
            if let Some(d) = proxy.filter(|_| self.proxiable_in(b.unwrap_or(&self.boxing), t)) {
                let pty = self.intern(TyKind::Adt(d, vec![]));
                if !out.contains(&pty) {
                    out.push(pty);
                }
            }
            return;
        }
        if !seen.insert(t) {
            return;
        }
        match self.kind(t) {
            TyKind::Option(e, _) | TyKind::Array(e) => self.refs_inline(b, e, out, seen),
            // A dynamic value may hold any counted type a dynamic value holds (issue #926);
            // the trace glue asks its descriptor (weak_object.rs). So may an interface value
            // whose tables keep `any` tables.
            TyKind::Dyn(d, _)
                if self.is_dynamic(t) || b.unwrap_or(&self.boxing).dyn_to_any.contains(&d) =>
            {
                let bx = b.unwrap_or(&self.boxing);
                if bx.weak_object {
                    let mut tys: Vec<TyId> = bx.dyn_types.iter().copied().collect();
                    tys.sort_by_key(|t| t.0);
                    for d in tys {
                        let counted = match b {
                            Some(b) => self.counted_in(b, d),
                            None => self.counted(d),
                        };
                        if counted && !out.contains(&d) {
                            out.push(d);
                        }
                    }
                }
            }
            // A function value: what the environments of the closures made at its key own
            // (glue/trace_env.rs). An environment is counted, so what it owns are the children
            // of a child; listing them here is what the weak-capable walk needs, and the trace
            // glue visits a function value exactly when this finds something.
            TyKind::FnPtr { .. } => {
                let closures = self.traced_closures(b, t);
                for (def, targs) in closures {
                    for (ct, _) in self.env_owned(def, &targs) {
                        self.refs_inline(b, ct, out, seen);
                    }
                }
            }
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
    /// glue visits). For a class with subclasses, what an object of any of its built classes
    /// refers to (glue/trace_class.rs).
    pub(in crate::lower) fn weak_refs_of(&mut self, b: Option<&Boxing>, t: TyId) -> Vec<TyId> {
        if !self.traceable(t) {
            let mut out: Vec<TyId> = vec![];
            for c in self.traced_classes(b, t) {
                for r in self.exact_refs_of(b, c) {
                    if !out.contains(&r) {
                        out.push(r);
                    }
                }
            }
            return out;
        }
        self.exact_refs_of(b, t)
    }

    /// The counted types an object whose type is exactly `t` refers to directly.
    pub(in crate::lower) fn exact_refs_of(&mut self, b: Option<&Boxing>, t: TyId) -> Vec<TyId> {
        let mut out = vec![];
        let mut seen = FxHashSet::default();
        let parts = match self.kind(t) {
            TyKind::Adt(..) if self.is_class(t) => self.adt_field_tys(t),
            TyKind::Array(e) => vec![e],
            _ => self.part_types(t),
        };
        for p in parts {
            self.refs_inline(b, p, &mut out, &mut seen);
        }
        for c in self.proxy_traced_as(b, t) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }

    /// For the `Proxy` class `t`: the counted types its target is traced as (glue/trace.rs,
    /// `Boxing::proxy_traced`); none for any other type.
    pub(in crate::lower) fn proxy_traced_as(&mut self, b: Option<&Boxing>, t: TyId) -> Vec<TyId> {
        let bx = b.unwrap_or(&self.boxing);
        let is_proxy = matches!(self.kind(t), TyKind::Adt(d, _) if Some(d) == bx.proxy_class);
        if !is_proxy {
            return vec![];
        }
        let cands: Vec<TyId> = bx.proxy_traced.iter().copied().collect();
        cands
            .into_iter()
            .filter(|&c| self.counted_in(b.unwrap_or(&self.boxing), c) && self.traceable(c))
            .collect()
    }

    /// The concrete classes of each hierarchy with a vtable that this pass built a vtable for
    /// (sorted, so the fixed point compares equal across passes): what a value of one of its
    /// classes may be (glue/trace_class.rs).
    fn classes_by_root(&mut self) -> rustc_hash::FxHashMap<velt_sema::hir::DefId, Vec<TyId>> {
        let mut built: Vec<TyId> = self.facts.vtable_types.iter().copied().collect();
        built.sort_by_key(|t| t.0);
        let mut out: rustc_hash::FxHashMap<velt_sema::hir::DefId, Vec<TyId>> = Default::default();
        for c in built {
            if let TyKind::Adt(d, _) = self.kind(c) {
                if self.is_class(c) && self.has_header(d) {
                    let root = self.class_root(d);
                    out.entry(root).or_default().push(c);
                }
            }
        }
        out
    }

    /// Display names of the weak-capable types (`VELT_DEBUG_COUNTED=1`).
    pub(in crate::lower) fn weak_names(&self) -> Vec<String> {
        let roots = self
            .boxing
            .weak_roots
            .iter()
            .map(|d| match self.hir.def(*d) {
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
        // In a program with weak references, the trace glue looks into the environments of the
        // closures made at each function type (glue/trace_env.rs), and every environment and
        // captured variable's cell may then be weakly held: their releases take the three-way
        // sequence (`Boxing::weak_envs`, program-wide, so no shape question depends on it).
        if !self.facts.weak_seeds.is_empty() || next.weak_object {
            next.env_traced = self.closures_by_key();
            next.weak_envs = !next.env_traced.is_empty();
            next.class_traced = self.classes_by_root();
        }
        let mut seeds: Vec<TyId> = self.facts.weak_seeds.iter().copied().collect();
        if next.weak_object {
            // Every type converted to a dynamic type may reach a weak `object` key.
            seeds.extend(next.dyn_types.iter().copied());
        }
        let mut work = vec![];
        for s in seeds {
            let refs = self.weak_refs_inline(Some(next), s);
            work.extend(refs);
        }
        // Only the weak sets change below; the counted types are fixed.
        let snapshot = next.clone();
        let mut seen = FxHashSet::default();
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
