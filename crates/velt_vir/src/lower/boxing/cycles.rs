//! Cycle-capable types (docs/internals/design/cycles.md section 1): the counted blocks that may
//! be part of a reference cycle, the only ones the cycle collector will look at.
//!
//! A cycle needs an object with two owners, so it contains at least one counted block: an
//! object of a counted type, or one of the blocks counted whatever their type (closure
//! environments on the heap, the cells of captured variables, the boxes interface values share
//! their data in). A uniquely owned object may sit on a cycle's path (a counted `A` owns an
//! uncounted `U` that refers back to `A`), so the walk goes through every type, counted or not.
//!
//! An edge `A -> B` means "a value of `A` may own a strong reference to a value of `B`". What a
//! function value, an interface value or a class with subclasses holds is not in its static
//! type, so those edges go to what the pass built:
//! - a function value to the environments of the closures made at its function type (the key,
//!   cycles_keys.rs), and to those of the generic closures; an environment to what it captures;
//! - an interface value to the types its implementation tables were built for, and to the
//!   interfaces converted to it; an `any`, `unknown` or `object` to the types `any` tables were
//!   built for, the interfaces converted to `any`, and the generic closures;
//! - a class with subclasses to every built subclass (strict: "is a" is not "owns");
//! - a generator object to the locals of the generator functions made at its class type (its
//!   state follows the class's fields);
//! - in a program that writes properties of functions through `any`, an environment to the
//!   dynamic values (its properties, which it owns).
//!
//! A `shared<T>` is a copy with an atomic count (out of scope), and a weak collection's entries
//! are the runtime's (its own fields and a subclass's are not), but a weak map's entry keeps its
//! value while its key lives: the ephemeron edge `K -> V`, from every type a key may be (a
//! subclass of a class key, the implementations behind an interface key, the concrete types
//! behind an `object` or `any` key).
//!
//! A counted block is cycle-capable when it lies on a cycle of that graph (cycles_scc.rs).
//! Classes are decided per hierarchy, by root class, as counting is. Nothing in code generation
//! uses the set yet: it is computed on demand, for `VELT_DEBUG_COUNTED=1`.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_sema::hir::{DefId, TyId, TyKind};

use super::cycles_keys::CycleKey;
use super::cycles_scc::{cycle_through, cyclic_components};
use crate::lower::Cx;

/// A node of the ownership graph.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Node {
    Ty(TyId),
    /// The environments of the closures made at a function type of this key (`None`: the
    /// generic closures, whose own type is not a function type of fixed types).
    Closures(Option<CycleKey>),
    /// The value of an interface value of this interface.
    Iface(DefId),
    /// The value of an `any`, `unknown` or `object`.
    Dynamics,
}

/// The cycle-capable blocks of a program.
#[derive(Debug, Default)]
pub(in crate::lower) struct CycleCapable {
    /// Class hierarchies, by root class.
    pub(in crate::lower) roots: FxHashSet<DefId>,
    /// Other counted types: arrays, object types, boxed values.
    pub(in crate::lower) tys: FxHashSet<TyId>,
    /// The closure environments (and their captured variables' cells), by key.
    pub(in crate::lower) closures: FxHashSet<Option<CycleKey>>,
    /// The boxes the values of these interfaces keep their data in.
    pub(in crate::lower) ifaces: FxHashSet<DefId>,
    /// The boxes dynamic values keep their data in.
    pub(in crate::lower) dynamics: bool,
}

/// The ownership graph, numbered as the walk reaches its nodes.
#[derive(Default)]
struct Graph {
    nodes: Vec<Node>,
    index: FxHashMap<Node, usize>,
    /// The ephemeron edges of the program's weak maps, `K -> V`, from every type a key may be.
    ephemerons: FxHashMap<TyId, Vec<TyId>>,
    /// What the generator objects of each generator class type hold: their functions' states.
    frames: FxHashMap<TyId, Vec<TyId>>,
    /// What the environments of each key capture.
    envs: FxHashMap<Option<CycleKey>, Vec<TyId>>,
    /// The concrete types each interface's tables were built for.
    impls: FxHashMap<DefId, Vec<TyId>>,
    /// The interfaces whose values were converted to each interface.
    upcast_from: FxHashMap<DefId, Vec<DefId>>,
}

impl Graph {
    fn id(&mut self, n: Node) -> usize {
        if let Some(&i) = self.index.get(&n) {
            return i;
        }
        let i = self.nodes.len();
        self.nodes.push(n.clone());
        self.index.insert(n, i);
        i
    }
}

impl Cx<'_> {
    /// The cycle-capable blocks of the program, as the counted types and this pass's facts
    /// stand (call it once they have settled). With `witness`, also a shortest cycle through each entry, as the names of the nodes it
    /// passes (`entry: a -> b -> a`).
    pub(in crate::lower) fn cycle_analysis(
        &mut self,
        witness: bool,
    ) -> (CycleCapable, Vec<String>) {
        let impls = self.iface_impl_types();
        let upcast_from = self.iface_upcasts();
        let mut g = Graph {
            ephemerons: self.ephemeron_edges(&impls, &upcast_from),
            frames: self.gen_frame_types(),
            envs: self.env_captures(),
            impls,
            upcast_from,
            ..Graph::default()
        };
        // Every counted block: objects of counted types, and the blocks counted whatever their
        // type, which the other nodes stand for.
        let mut starts: Vec<Node> = (self.table_types().into_iter())
            .filter(|&t| self.counted(t))
            .map(Node::Ty)
            .collect();
        let mut keys: Vec<Option<CycleKey>> = g.envs.keys().cloned().collect();
        keys.sort_by_key(|k| format!("{k:?}"));
        starts.extend(keys.into_iter().map(Node::Closures));
        let mut ifaces: Vec<DefId> = g.impls.keys().copied().collect();
        ifaces.sort_by_key(|d| d.0);
        starts.extend(ifaces.into_iter().map(Node::Iface));
        starts.push(Node::Dynamics);
        let starts: Vec<usize> = starts.into_iter().map(|n| g.id(n)).collect();
        let (comps, edges) = cyclic_components(&starts, &mut |v| {
            let n = g.nodes[v].clone();
            let owned = self.owned(&g, n);
            owned.into_iter().map(|n| g.id(n)).collect()
        });
        let mut witnesses = vec![];
        for comp in comps.iter().filter(|_| witness) {
            let members: std::collections::HashSet<usize> = comp.iter().copied().collect();
            for &i in comp {
                if matches!(g.nodes[i], Node::Ty(t) if !self.counted(t)) {
                    continue;
                }
                let path = cycle_through(i, &members, &edges);
                let names: Vec<String> = (path.iter().chain(path.first()))
                    .map(|&n| self.node_name(&g.nodes[n]))
                    .collect();
                let entry = self.node_name(&g.nodes[i]);
                witnesses.push(format!("{entry}: {}", names.join(" -> ")));
            }
        }
        witnesses.sort();
        witnesses.dedup();
        let mut out = CycleCapable::default();
        for i in comps.into_iter().flatten() {
            match g.nodes[i].clone() {
                Node::Ty(t) if self.counted(t) => match self.kind(t) {
                    TyKind::Adt(d, _) if self.is_class(t) => {
                        out.roots.insert(self.class_root(d));
                    }
                    _ => {
                        out.tys.insert(t);
                    }
                },
                Node::Ty(_) => {}
                Node::Closures(k) => {
                    out.closures.insert(k);
                }
                Node::Iface(d) => {
                    out.ifaces.insert(d);
                }
                Node::Dynamics => out.dynamics = true,
            }
        }
        (out, witnesses)
    }

    /// How a node of the ownership graph is named in a witness cycle.
    fn node_name(&mut self, n: &Node) -> String {
        match n {
            Node::Ty(t) => match self.kind(*t) {
                TyKind::Adt(d, _) if self.is_class(*t) => self.class_name(d),
                _ => self.type_name(*t),
            },
            Node::Closures(k) => self.closure_set_name(k),
            Node::Iface(d) => self.iface_entry_name(*d),
            Node::Dynamics => "dynamic values".into(),
        }
    }

    /// Display names of the cycle-capable blocks, a line of sites per entry (cycles_sites.rs)
    /// and a shortest cycle per component (`VELT_DEBUG_COUNTED=1`).
    pub(in crate::lower) fn cycle_report(&mut self) -> (Vec<String>, Vec<String>, Vec<String>) {
        let (c, witnesses) = self.cycle_analysis(true);
        let mut names: Vec<String> = c.roots.iter().map(|&d| self.class_name(d)).collect();
        let tys: Vec<TyId> = c.tys.iter().copied().collect();
        names.extend(tys.into_iter().map(|t| self.type_name(t)));
        let keys: Vec<Option<CycleKey>> = c.closures.iter().cloned().collect();
        names.extend(keys.iter().map(|k| self.closure_set_name(k)));
        names.extend(c.ifaces.iter().map(|&d| self.iface_entry_name(d)));
        if c.dynamics {
            names.push("dynamic values".into());
        }
        names.sort();
        let sites = self.cycle_site_lines(&c);
        (names, sites, witnesses)
    }

    /// How the boxes of interface `d`'s values are named.
    pub(in crate::lower) fn iface_entry_name(&self, d: DefId) -> String {
        format!("`{}` interface values", self.adt_or_iface_name(d))
    }

    /// `class Name` of the class `d`.
    pub(in crate::lower) fn class_name(&self, d: DefId) -> String {
        format!("class {}", self.adt_or_iface_name(d))
    }

    /// The name of the class or interface `d`.
    fn adt_or_iface_name(&self, d: DefId) -> String {
        match self.hir.def(d) {
            velt_sema::hir::Def::Adt(a) => a.name.to_string(),
            velt_sema::hir::Def::Interface(i) => i.name.to_string(),
            _ => "?".into(),
        }
    }

    /// How the environments of key `k` are named.
    pub(in crate::lower) fn closure_set_name(&mut self, k: &Option<CycleKey>) -> String {
        match k {
            Some(CycleKey::Fixed(params, ret, throws, rest)) => {
                let t = self.intern(TyKind::FnPtr {
                    params: params.clone(),
                    ret: *ret,
                    throws: *throws,
                    rest: *rest,
                });
                format!("closure environments of `{}`", self.type_name(t))
            }
            Some(CycleKey::Generic(d)) => format!(
                "closure environments of generic `{}`",
                self.adt_or_iface_name(*d)
            ),
            None => "closure environments of generic closures".into(),
        }
    }

    /// Every type of the table that is a concrete type of this program (not a shape
    /// placeholder, nor a type mentioning one).
    fn table_types(&self) -> Vec<TyId> {
        // A placeholder mentions itself, so this leaves out placeholders too.
        (0..self.types.len() as u32)
            .map(TyId)
            .filter(|&t| !self.mentions_ph(t))
            .collect()
    }

    /// `K -> [V]` for every `WeakMap<K, V>` type of the program: an entry keeps its value
    /// alive while its key lives.
    fn ephemeron_edges(
        &mut self,
        impls: &FxHashMap<DefId, Vec<TyId>>,
        upcast_from: &FxHashMap<DefId, Vec<DefId>>,
    ) -> FxHashMap<TyId, Vec<TyId>> {
        let mut out: FxHashMap<TyId, Vec<TyId>> = FxHashMap::default();
        for t in self.table_types() {
            if self.weak_class(t) != Some("WeakMap") {
                continue;
            }
            // The prelude class itself, not a subclass, whose type arguments are its own.
            let TyKind::Adt(d, args) = self.kind(t) else {
                continue;
            };
            if self.class_root(d) != d {
                continue;
            }
            if let [k, v] = args[..] {
                // The entry lives as long as the key object, whatever its dynamic type.
                for kt in self.key_types(k, impls, upcast_from) {
                    push_new(out.entry(kt).or_default(), v);
                }
            }
        }
        out
    }

    /// The types a weak map key of static type `k` may be: `k` itself, and the built strict
    /// subclasses of a class, the implementations of an interface (and of the interfaces
    /// converted to it), or the concrete types behind `object` and `any`.
    fn key_types(
        &mut self,
        k: TyId,
        impls: &FxHashMap<DefId, Vec<TyId>>,
        upcast_from: &FxHashMap<DefId, Vec<DefId>>,
    ) -> Vec<TyId> {
        let mut out = vec![k];
        match self.kind(k) {
            TyKind::Adt(d, _) if self.is_class(k) && self.has_header(d) => {
                for c in sorted(&self.facts.vtable_types) {
                    if matches!(self.kind(c), TyKind::Adt(cd, _)
                        if self.is_class(c) && self.strict_subclass(cd, d))
                    {
                        out.push(c);
                    }
                }
            }
            TyKind::Dyn(..) if self.is_dynamic(k) => {
                out.extend(sorted(&self.facts.dyn_types));
                out.extend(sorted(&self.facts.object_types));
                let mut all: Vec<TyId> = impls.values().flatten().copied().collect();
                all.sort_by_key(|t| t.0);
                out.extend(all);
            }
            TyKind::Dyn(d, _) => {
                let mut ifaces = vec![d];
                let mut i = 0;
                while let Some(&cur) = ifaces.get(i) {
                    for &f in upcast_from.get(&cur).map_or(&[][..], |v| &v[..]) {
                        if !ifaces.contains(&f) {
                            ifaces.push(f);
                        }
                    }
                    i += 1;
                }
                for f in ifaces {
                    out.extend(impls.get(&f).map_or(&[][..], |v| &v[..]).iter().copied());
                }
            }
            _ => {}
        }
        out
    }

    /// For each generator class type, the types its objects' states hold: every local of the
    /// generator functions made at that type (a state keeps those live across a `yield`; all
    /// of them is an over-approximation), substituted.
    pub(in crate::lower) fn gen_frame_types(&mut self) -> FxHashMap<TyId, Vec<TyId>> {
        let mut frames: Vec<_> = self.facts.gen_frames.iter().cloned().collect();
        frames.sort_by_key(|f| format!("{f:?}"));
        let mut out: FxHashMap<TyId, Vec<TyId>> = FxHashMap::default();
        for (g, def, targs) in frames {
            let locals: Vec<TyId> = self.fn_def(def).body.locals.iter().map(|l| l.ty).collect();
            for t in locals {
                let t = self.subst(t, &targs);
                push_new(out.entry(g).or_default(), t);
            }
        }
        out
    }

    /// Record that a generator object of class type `g` holds the state of generator `def` at
    /// `targs`.
    pub(in crate::lower) fn note_gen_frame(&mut self, g: TyId, def: DefId, targs: &[TyId]) {
        // Read by the cycle analysis and, in a program with weak references, by the trace glue
        // of generator objects (async_fn/gen_trace.rs).
        if !self.cycle_facts && !self.has_weak() {
            return;
        }
        (self.facts.gen_frames).insert((g, def, targs.to_vec()));
    }

    /// What the environments of each key capture: the types of their captured variables (a
    /// cell holds a value of the variable's type), substituted.
    fn env_captures(&mut self) -> FxHashMap<Option<CycleKey>, Vec<TyId>> {
        let mut envs: Vec<_> = self.facts.closure_envs.iter().cloned().collect();
        envs.sort_by_key(|e| format!("{e:?}"));
        let mut out: FxHashMap<Option<CycleKey>, Vec<TyId>> = FxHashMap::default();
        for (key, def, targs) in envs {
            let f = self.fn_def(def);
            let caps: Vec<TyId> = (f.captures.iter())
                .map(|c| f.body.locals[c.inner.0 as usize].ty)
                .collect();
            let caps: Vec<TyId> = caps.into_iter().map(|t| self.subst(t, &targs)).collect();
            let to = out.entry(key).or_default();
            for t in caps {
                push_new(to, t);
            }
        }
        out
    }

    /// The concrete types each interface's tables were built for.
    fn iface_impl_types(&self) -> FxHashMap<DefId, Vec<TyId>> {
        let mut pairs: Vec<(DefId, TyId)> = self
            .facts
            .iface_impls
            .iter()
            .map(|&(d, _, t)| (d, t))
            .collect();
        pairs.sort_by_key(|(d, t)| (d.0, t.0));
        let mut out: FxHashMap<DefId, Vec<TyId>> = FxHashMap::default();
        for (d, t) in pairs {
            push_new(out.entry(d).or_default(), t);
        }
        out
    }

    /// For each interface, the interfaces whose values were converted to it (`Upcast`).
    fn iface_upcasts(&self) -> FxHashMap<DefId, Vec<DefId>> {
        let mut pairs: Vec<(TyId, TyId)> = self.facts.upcasts.iter().copied().collect();
        pairs.sort_by_key(|(a, b)| (a.0, b.0));
        let mut out: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
        for (from, to) in pairs {
            if let (TyKind::Dyn(f, _), TyKind::Dyn(t, _)) = (self.kind(from), self.kind(to)) {
                push_new(out.entry(t).or_default(), f);
            }
        }
        out
    }

    /// The nodes a value of `n` may own a strong reference to.
    fn owned(&mut self, g: &Graph, n: Node) -> Vec<Node> {
        let mut out: Vec<Node> = vec![];
        match n {
            Node::Closures(k) => {
                let tys = g.envs.get(&k).cloned().unwrap_or_default();
                out.extend(tys.into_iter().map(Node::Ty));
                // A function's properties (`f.self = f` through `any`) are `any` values its
                // environment owns (velt_rt `fn_props.rs`).
                if self.boxing.fn_props {
                    out.push(Node::Dynamics);
                }
            }
            Node::Iface(d) => {
                let tys = g.impls.get(&d).cloned().unwrap_or_default();
                out.extend(tys.into_iter().map(Node::Ty));
                // The interfaces converted to `d`. Today redundant: an upcast builds `d`'s table
                // for each concrete type behind the converted interface (glue/vtable.rs, the
                // targets of `upcast_impls`), so `impls[d]` already lists those types. Kept so the
                // analysis stays sound if an upcast ever reuses the source's table instead.
                let from = g.upcast_from.get(&d).cloned().unwrap_or_default();
                out.extend(from.into_iter().map(Node::Iface));
            }
            Node::Dynamics => {
                out.extend(sorted(&self.facts.dyn_types).into_iter().map(Node::Ty));
                let mut to_any: Vec<DefId> = self.facts.dyn_to_any.iter().copied().collect();
                to_any.sort_by_key(|d| d.0);
                out.extend(to_any.into_iter().map(Node::Iface));
                // A generic closure held as `unknown` (glue/vtable.rs `ClosureAny`).
                out.push(Node::Closures(None));
            }
            Node::Ty(t) => {
                match self.kind(t) {
                    TyKind::FnPtr { .. } => {
                        out.push(Node::Closures(self.cycle_key(t)));
                        out.push(Node::Closures(None));
                    }
                    TyKind::Closure(_) => out.push(Node::Closures(None)),
                    TyKind::Dyn(d, _) if self.is_generic_fn_value(t) => {
                        out.push(Node::Closures(Some(CycleKey::Generic(d))));
                        out.push(Node::Closures(None));
                        out.push(Node::Iface(d));
                    }
                    TyKind::Dyn(..) if self.is_dynamic(t) => out.push(Node::Dynamics),
                    TyKind::Dyn(d, _) => out.push(Node::Iface(d)),
                    _ => {}
                }
                out.extend(self.owned_tys(t).into_iter().map(Node::Ty));
                if let Some(vs) = g.ephemerons.get(&t) {
                    out.extend(vs.iter().copied().map(Node::Ty));
                }
                // A generator object holds its function's state after the class's fields.
                if let Some(fs) = g.frames.get(&t) {
                    out.extend(fs.iter().copied().map(Node::Ty));
                }
            }
        }
        out
    }

    /// The types a value of `t` owns directly, by its static type.
    fn owned_tys(&mut self, t: TyId) -> Vec<TyId> {
        match self.kind(t) {
            // A `shared<T>` is a copy with an atomic count of its own.
            TyKind::Shared(_) => vec![],
            TyKind::Array(e) | TyKind::Option(e, _) => vec![e],
            TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => vec![a, b],
            TyKind::Tuple(es) => es,
            TyKind::Adt(d, args) => {
                // A class's weak fields are no edges (weak_fields.rs).
                let mut tys = match self.is_class(t) {
                    true => self.adt_edge_tys(t),
                    false => self.part_types(t),
                };
                // What a standard-library container keeps behind its own pointers; but a weak
                // collection's entries are the runtime's (the ephemeron edges stand for them),
                // while its fields and a subclass's are its own.
                if self.weak_class(t).is_none() {
                    tys.extend(args);
                }
                if self.is_class(t) && self.has_header(d) {
                    // It may be an object of any subclass that was built, which owns what the
                    // subclass's fields hold. Only strict subclasses: "is a" is not "owns", and
                    // an edge to the class itself or to a base would close a cycle no object
                    // graph has.
                    for c in sorted(&self.facts.vtable_types) {
                        if matches!(self.kind(c), TyKind::Adt(cd, _)
                            if self.is_class(c) && self.strict_subclass(cd, d))
                        {
                            tys.push(c);
                        }
                    }
                }
                tys
            }
            _ => vec![],
        }
    }
}

impl Cx<'_> {
    /// Does the class `sub` extend `sup`, directly or not (and is not `sup` itself)?
    pub(in crate::lower) fn strict_subclass(&self, mut sub: DefId, sup: DefId) -> bool {
        while let Some(b) = self.adt_def(sub).base {
            let TyKind::Adt(bd, _) = self.kind(b) else {
                return false;
            };
            if bd == sup {
                return true;
            }
            sub = bd;
        }
        false
    }
}

fn sorted(s: &FxHashSet<TyId>) -> Vec<TyId> {
    let mut v: Vec<TyId> = s.iter().copied().collect();
    v.sort_by_key(|t| t.0);
    v
}

fn push_new<T: PartialEq>(v: &mut Vec<T>, t: T) {
    if !v.contains(&t) {
        v.push(t);
    }
}
