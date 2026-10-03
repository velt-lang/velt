//! Dispatch groups: functions that are called through one dynamic entry must agree on their
//! error type (it is part of the ABI: a throwing function returns `Result<ret, E>`). A group is
//! an interface method slot with its default and every implementation, and a class vtable slot
//! with the base method and every override (linked when a method is in both).
//!
//! A group's error type is the interface method's (or the base method's) `throws` clause when
//! written — the implementations may throw only what it allows — else the union of what the
//! members throw (inferred). For an interface method or a base class method returning a promise
//! it is what the promises reject with (implemented by async methods), as for async functions.
//! Group error types cannot mention type parameters (every member would see them differently).

use std::collections::HashMap;

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::defs::{DeclaredThrows, DefInfo};
use crate::hir::{DefId, TyId};

/// A node of the union-find: a function, or an interface method slot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Node {
    Def(DefId),
    Slot(DefId, u32),
}

/// A written `throws` that bounds a group, and where it comes from (for messages).
#[derive(Clone, Debug)]
pub(crate) struct GroupBound {
    pub decl: DeclaredThrows,
    /// "`I.m`" / "`Base.m`".
    pub owner: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Group {
    pub members: Vec<DefId>,
    pub bound: Option<GroupBound>,
    /// It holds an interface method or a vtable slot returning a promise: its error type is
    /// what the promises reject with, and calling an entry never throws (async members; a
    /// synchronous member may only forward to an async one).
    pub promise: bool,
    /// The method every member implements or overrides (for messages).
    pub owner: Option<GroupOwner>,
}

/// The interface method or base class method of a group.
#[derive(Clone, Debug)]
pub(crate) struct GroupOwner {
    /// `I.m` / `Base.m`.
    pub name: String,
    pub interface: bool,
    /// Its return type as declared (a type parameter for `get(): T`).
    pub ret: TyId,
}

/// Every dispatch group of the program.
#[derive(Clone, Debug, Default)]
pub(crate) struct Groups {
    of: HashMap<DefId, usize>,
    slots: HashMap<(DefId, u32), usize>,
    pub list: Vec<Group>,
}

impl Groups {
    /// The group of function `d`, if it is dispatched dynamically.
    pub fn group_of(&self, d: DefId) -> Option<usize> {
        self.of.get(&d).copied()
    }

    /// Does `d` belong to a promise group (see [`Group::promise`])?
    pub fn in_promise_group(&self, d: DefId) -> bool {
        self.group_of(d).is_some_and(|g| self.list[g].promise)
    }

    /// The group of interface method `slot` of `iface`.
    pub fn slot_group(&self, iface: DefId, slot: u32) -> Option<usize> {
        self.slots.get(&(iface, slot)).copied()
    }
}

struct UnionFind {
    ids: HashMap<Node, usize>,
    nodes: Vec<Node>,
    parent: Vec<usize>,
}

impl UnionFind {
    fn id(&mut self, n: Node) -> usize {
        if let Some(&i) = self.ids.get(&n) {
            return i;
        }
        let i = self.nodes.len();
        self.ids.insert(n, i);
        self.nodes.push(n);
        self.parent.push(i);
        i
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn union(&mut self, a: Node, b: Node) {
        let (a, b) = (self.id(a), self.id(b));
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

/// Build the groups; reports conflicting or generic bounds.
pub(crate) fn build(cx: &mut Ctx) -> Groups {
    let mut uf = UnionFind {
        ids: HashMap::new(),
        nodes: vec![],
        parent: vec![],
    };
    let mut bounds: Vec<(Node, GroupBound)> = vec![];
    link_interfaces(cx, &mut uf, &mut bounds);
    let roots = link_vtables(cx, &mut uf, &mut bounds);
    let mut groups = Groups::default();
    let mut by_root: HashMap<usize, usize> = HashMap::new();
    for i in 0..uf.nodes.len() {
        let r = uf.find(i);
        let g = *by_root.entry(r).or_insert_with(|| {
            groups.list.push(Group::default());
            groups.list.len() - 1
        });
        match uf.nodes[i] {
            Node::Def(d) => {
                groups.of.insert(d, g);
                groups.list[g].members.push(d);
            }
            Node::Slot(iface, s) => {
                groups.slots.insert((iface, s), g);
                let Some(i) = cx.iface(iface) else { continue };
                let Some(m) = i.methods.get(s as usize) else {
                    continue;
                };
                let owner = GroupOwner {
                    name: format!("{}.{}", i.name, m.name),
                    interface: true,
                    ret: m.ret,
                };
                if cx.ty.promise_payload(owner.ret).is_some() {
                    groups.list[g].promise = true;
                }
                groups.list[g].owner = Some(owner);
            }
        }
    }
    for root in roots {
        let g = &mut groups.list[groups.of[&root]];
        if g.owner.as_ref().is_some_and(|o| o.interface) {
            continue;
        }
        let f = cx.fn_info(root);
        let owner = GroupOwner {
            name: f.name.rsplit("::").next().unwrap_or(&f.name).to_string(),
            interface: false,
            ret: f.ret,
        };
        // Getters cannot be `async`: a getter's errors are thrown, whatever it returns.
        if !f.is_getter && cx.ty.promise_payload(owner.ret).is_some() {
            g.promise = true;
        }
        g.owner = Some(owner);
    }
    for (n, b) in bounds {
        let i = uf.id(n);
        let g = by_root[&uf.find(i)];
        set_bound(cx, &mut groups.list[g], b);
    }
    groups
}

fn set_bound(cx: &mut Ctx, g: &mut Group, b: GroupBound) {
    if let Some(t) = b.decl.ty {
        if cx.mentions_params(t) {
            cx.error(
                Diagnostic::error(
                    "the `throws` clause of an interface method or an overridden method cannot mention type parameters",
                    b.decl.span,
                )
                .with_note("implementations are called through one dynamic entry, which needs one error type"),
            );
            return;
        }
    }
    match &g.bound {
        None => g.bound = Some(b),
        Some(old) => {
            let (a, c) = (cx.canon_error(old.decl.ty), cx.canon_error(b.decl.ty));
            if a != c {
                cx.error(
                    Diagnostic::error(
                        format!(
                            "`{}` and `{}` declare different `throws` for methods dispatched together",
                            old.owner, b.owner
                        ),
                        b.decl.span,
                    )
                    .with_label(old.decl.span, "the other declaration"),
                );
            }
        }
    }
}

fn link_interfaces(cx: &mut Ctx, uf: &mut UnionFind, bounds: &mut Vec<(Node, GroupBound)>) {
    let impls: Vec<(DefId, Vec<DefId>)> = cx
        .impls
        .iter()
        .map(|imp| (imp.iface, imp.methods.clone()))
        .collect();
    for (iface, methods) in impls {
        for (slot, m) in methods.into_iter().enumerate() {
            uf.union(Node::Slot(iface, slot as u32), Node::Def(m));
        }
    }
    for i in 0..cx.info.len() {
        let DefInfo::Iface(info) = &cx.info[i] else {
            continue;
        };
        let iface = DefId(i as u32);
        for (slot, m) in info.methods.iter().enumerate() {
            let node = Node::Slot(iface, slot as u32);
            uf.id(node);
            if let Some(d) = m.default {
                uf.union(node, Node::Def(d));
            }
            if let Some(decl) = m.throws {
                let owner = format!("{}.{}", info.name, m.name);
                bounds.push((node, GroupBound { decl, owner }));
            }
        }
    }
}

/// Link every vtable slot's base method with its overrides; returns the base methods (the
/// slots each class introduces).
fn link_vtables(
    cx: &mut Ctx,
    uf: &mut UnionFind,
    bounds: &mut Vec<(Node, GroupBound)>,
) -> Vec<DefId> {
    let mut roots = vec![];
    for i in 0..cx.info.len() {
        let DefInfo::Adt(a) = &cx.info[i] else {
            continue;
        };
        let vtable = a.vtable.clone();
        let base_vtable = a
            .base
            .and_then(|b| cx.class_of(b))
            .and_then(|(bd, _)| cx.adt(bd).map(|b| b.vtable.clone()))
            .unwrap_or_default();
        for (slot, &m) in vtable.iter().enumerate() {
            uf.id(Node::Def(m));
            match base_vtable.get(slot) {
                Some(&b) => uf.union(Node::Def(b), Node::Def(m)),
                None => {
                    if let Some(decl) = cx.try_fn(m).and_then(|f| f.declared_throws) {
                        let owner = cx.fn_info(m).name.clone();
                        bounds.push((Node::Def(m), GroupBound { decl, owner }));
                    }
                    if cx.try_fn(m).is_some() {
                        roots.push(m);
                    }
                }
            }
        }
    }
    roots
}
