//! Cycle-capable types (docs/internals/design/cycles.md section 1): the counted types whose
//! objects may be part of a reference cycle, the only ones the cycle collector will look at.
//!
//! A cycle needs an object with two owners, so it contains at least one counted block: an
//! object of a counted type, or one of the blocks counted whatever their type (closure
//! environments on the heap, the cells of captured variables, the boxes interface values share
//! their data in). A uniquely owned object may sit on a cycle's path (a counted `A` owns an
//! uncounted `U` that refers back to `A`), so the walk goes through every type, counted or not. An edge `A -> B` means "a
//! value of `A` may own a strong reference to a value of `B`". What a function value, an
//! interface value or a class with subclasses holds is not in its static type, so those edges
//! go to what the pass actually built, as `transfers.rs` and `module_reach.rs` walk them: the
//! capture types of every closure environment, and the concrete types put into interface and
//! `any` tables. A `shared<T>` is a copy with an atomic count (out of scope), and a weak
//! collection holds nothing strongly, but a weak map's entry keeps its value while its key
//! lives: the ephemeron edge `K -> V`.
//!
//! A counted type is cycle-capable when it lies on a cycle of that graph (a strongly connected
//! component with two nodes or more, or an edge to itself). Classes are decided per hierarchy,
//! by root class, as counting is. Nothing in code generation uses the set yet (step 1 of the
//! design): it is computed on demand, for `VELT_DEBUG_COUNTED=1`.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_sema::hir::{DefId, TyId, TyKind};

use crate::lower::shape::SHAPE_BASE;
use crate::lower::Cx;

/// A node of the ownership graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Node {
    Ty(TyId),
    /// The environment of any closure of the program: a function value may be any of them.
    Closures,
    /// The concrete value of any interface or dynamic value of the program.
    Dynamics,
}

/// The cycle-capable types of a program.
#[derive(Debug, Default)]
pub(in crate::lower) struct CycleCapable {
    /// Class hierarchies, by root class.
    pub(in crate::lower) roots: FxHashSet<DefId>,
    /// Other counted types: arrays, object types, boxed values.
    pub(in crate::lower) tys: FxHashSet<TyId>,
    /// Closure environments and captured variables' cells may be in a cycle (all of them: a
    /// function value may be any closure of the program).
    pub(in crate::lower) closures: bool,
    /// The boxes interface and dynamic values keep their data in may be in a cycle.
    pub(in crate::lower) dynamics: bool,
}

/// The ownership graph, built as Tarjan's walk reaches its nodes.
#[derive(Default)]
struct Graph {
    nodes: Vec<Node>,
    index: FxHashMap<Node, usize>,
    succ: Vec<Vec<usize>>,
    /// The ephemeron edges of the program's weak maps, `K -> V`.
    ephemerons: FxHashMap<TyId, Vec<TyId>>,
}

impl Graph {
    fn id(&mut self, n: Node) -> (usize, bool) {
        if let Some(&i) = self.index.get(&n) {
            return (i, false);
        }
        let i = self.nodes.len();
        self.nodes.push(n);
        self.index.insert(n, i);
        self.succ.push(vec![]);
        (i, true)
    }
}

impl Cx<'_> {
    /// The cycle-capable types of the program, as the counted types and this pass's facts
    /// stand (call it once they have settled).
    pub(in crate::lower) fn cycle_capable(&mut self) -> CycleCapable {
        let mut g = Graph {
            ephemerons: self.ephemeron_edges(),
            ..Graph::default()
        };
        // Every counted block: objects of counted types, and the blocks counted whatever their
        // type, which the two summary nodes stand for.
        let mut starts: Vec<Node> = self
            .table_types()
            .into_iter()
            .filter(|&t| self.counted(t))
            .map(Node::Ty)
            .collect();
        starts.extend([Node::Closures, Node::Dynamics]);
        let mut out = CycleCapable::default();
        for comp in self.cyclic_components(&mut g, &starts) {
            for i in comp {
                let t = match g.nodes[i] {
                    Node::Ty(t) => t,
                    Node::Closures => {
                        out.closures = true;
                        continue;
                    }
                    Node::Dynamics => {
                        out.dynamics = true;
                        continue;
                    }
                };
                if !self.counted(t) {
                    continue;
                }
                match self.kind(t) {
                    TyKind::Adt(d, _) if self.is_class(t) => {
                        out.roots.insert(self.class_root(d));
                    }
                    _ => {
                        out.tys.insert(t);
                    }
                }
            }
        }
        out
    }

    /// Display names of the cycle-capable types (`VELT_DEBUG_COUNTED=1`).
    pub(in crate::lower) fn cycle_names(&mut self) -> Vec<String> {
        let c = self.cycle_capable();
        let roots = c.roots.iter().map(|d| match self.hir.def(*d) {
            velt_sema::hir::Def::Adt(a) => format!("class {}", a.name),
            _ => "?".into(),
        });
        let tys = c.tys.iter().map(|&t| self.type_name(t));
        let blocks = [
            c.closures.then(|| "closure environments".to_string()),
            c.dynamics
                .then(|| "interface and dynamic values".to_string()),
        ];
        let mut out: Vec<String> = roots
            .chain(tys)
            .chain(blocks.into_iter().flatten())
            .collect();
        out.sort();
        out
    }

    /// Every type of the table that is a concrete type of this program (not a shape
    /// placeholder, nor a type mentioning one).
    fn table_types(&self) -> Vec<TyId> {
        (0..self.types.len() as u32)
            .map(TyId)
            .filter(|&t| {
                !matches!(self.types.kind(t), TyKind::Param(n) if *n >= SHAPE_BASE)
                    && !self.mentions_ph(t)
            })
            .collect()
    }

    /// `K -> [V]` for every `WeakMap<K, V>` type of the program: an entry keeps its value
    /// alive while its key lives.
    fn ephemeron_edges(&mut self) -> FxHashMap<TyId, Vec<TyId>> {
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
                let vs = out.entry(k).or_default();
                if !vs.contains(&v) {
                    vs.push(v);
                }
            }
        }
        out
    }

    /// The nodes a value of `n` may own a strong reference to.
    fn owned(&mut self, g: &Graph, n: Node) -> Vec<Node> {
        let tys: Vec<TyId> = match n {
            Node::Closures => sorted(&self.facts.captures),
            Node::Dynamics => {
                let mut tys = sorted(&self.facts.vtable_types);
                tys.extend(sorted(&self.facts.dyn_types));
                tys
            }
            Node::Ty(t) => {
                let mut tys = self.owned_tys(t);
                if let Some(vs) = g.ephemerons.get(&t) {
                    tys.extend(vs.iter().copied());
                }
                match self.kind(t) {
                    TyKind::Closure(_) | TyKind::FnPtr { .. } => {
                        return [Node::Closures].into_iter().chain(ty_nodes(tys)).collect();
                    }
                    TyKind::Dyn(..) => {
                        return [Node::Dynamics].into_iter().chain(ty_nodes(tys)).collect();
                    }
                    _ => {}
                }
                tys
            }
        };
        ty_nodes(tys).collect()
    }

    /// The types a value of `t` owns directly, by its static type.
    fn owned_tys(&mut self, t: TyId) -> Vec<TyId> {
        match self.kind(t) {
            // A `shared<T>` is a copy with an atomic count of its own.
            TyKind::Shared(_) => vec![],
            TyKind::Array(e) | TyKind::Option(e, _) => vec![e],
            TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => vec![a, b],
            TyKind::Tuple(es) => es,
            // A weak collection's entries are the runtime's; the ephemeron edges stand for them.
            TyKind::Adt(..) if self.weak_class(t).is_some() => vec![],
            TyKind::Adt(d, args) => {
                let mut tys = self.part_types(t);
                // What a standard-library container keeps behind its own pointers.
                tys.extend(args);
                if self.is_class(t) && self.has_header(d) {
                    // It may hold any class of its hierarchy that was built.
                    let root = self.class_root(d);
                    for c in sorted(&self.facts.vtable_types) {
                        if matches!(self.kind(c), TyKind::Adt(cd, _)
                            if self.is_class(c) && self.class_root(cd) == root)
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

    /// The strongly connected components reachable from `starts` that contain a cycle (two
    /// nodes or more, or one with an edge to itself), by Tarjan's algorithm without recursion.
    fn cyclic_components(&mut self, g: &mut Graph, starts: &[Node]) -> Vec<Vec<usize>> {
        struct Frame {
            node: usize,
            next: usize,
        }
        let mut order: Vec<Option<usize>> = vec![];
        let mut low: Vec<usize> = vec![];
        let mut on_stack: Vec<bool> = vec![];
        let mut stack: Vec<usize> = vec![];
        let mut out = vec![];
        let mut counter = 0;
        for &s in starts {
            let (root, new) = g.id(s);
            if !new && order.get(root).copied().flatten().is_some() {
                continue;
            }
            let mut frames = vec![Frame {
                node: root,
                next: 0,
            }];
            while let Some(f) = frames.last_mut() {
                let v = f.node;
                if order.len() <= v {
                    order.resize(v + 1, None);
                    low.resize(v + 1, 0);
                    on_stack.resize(v + 1, false);
                }
                if order[v].is_none() {
                    order[v] = Some(counter);
                    low[v] = counter;
                    counter += 1;
                    stack.push(v);
                    on_stack[v] = true;
                    let owned = self.owned(g, g.nodes[v]);
                    let ids: Vec<usize> = owned.into_iter().map(|n| g.id(n).0).collect();
                    g.succ[v] = ids;
                }
                let f = frames.last_mut().expect("ICE: a Tarjan frame vanished");
                if let Some(&w) = g.succ[v].get(f.next) {
                    f.next += 1;
                    match order.get(w).copied().flatten() {
                        None => frames.push(Frame { node: w, next: 0 }),
                        Some(ow) if on_stack[w] => low[v] = low[v].min(ow),
                        Some(_) => {}
                    }
                    continue;
                }
                frames.pop();
                if let Some(parent) = frames.last() {
                    let p = parent.node;
                    low[p] = low[p].min(low[v]);
                }
                if Some(low[v]) == order[v] {
                    let mut comp = vec![];
                    loop {
                        let w = stack.pop().expect("ICE: Tarjan's stack ran out");
                        on_stack[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    if comp.len() > 1 || g.succ[v].contains(&v) {
                        out.push(comp);
                    }
                }
            }
        }
        out
    }
}

fn sorted(s: &FxHashSet<TyId>) -> Vec<TyId> {
    let mut v: Vec<TyId> = s.iter().copied().collect();
    v.sort_by_key(|t| t.0);
    v
}

fn ty_nodes(tys: Vec<TyId>) -> impl Iterator<Item = Node> {
    tys.into_iter().map(Node::Ty)
}
