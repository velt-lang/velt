//! Dispatch groups: functions that are called through one dynamic entry must agree on their
//! error type (it is part of the ABI: a throwing function returns `Result<ret, E>`). A group is
//! an interface method slot with its default and every implementation, and a class vtable slot
//! with the base method and every override (linked when a method is in both). A method slot of
//! an interface whose values convert to another (`synth_ifaces` upcasts) is in the group of the
//! other's method of that name: the target's table holds the same implementations, made only
//! after this inference (`Ctx::upcast_tables`).
//!
//! A group's error type is the interface method's (or the base method's) `throws` clause when
//! written — the implementations may throw only what it allows — else the union of what the
//! members throw (inferred). For an interface method or a base class method returning a promise
//! it is what the promises reject with (implemented by async methods), as for async functions.
//! An inferred group error type cannot mention type parameters (every member would see them
//! differently). A generator method is no member: calling it only creates the generator and
//! never throws (its error type is the `E` of its result, `Iterator<T, E>`), so it neither adds
//! to nor takes the group's error type. A written bound on a generic interface's method may
//! mention the interface's own parameters (`next(): IteratorResult<T> throws E` in
//! `Iterator<T, E>`): each member then sees it with the interface arguments of its
//! implementation ([`member_bound`]), and a call through an interface value with that value's
//! arguments; vtable entries of different instantiations never mix, because each
//! `Iterator<T, E>` instantiation has its own vtable.

use rustc_hash::FxHashMap;

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
    /// The interface whose method declares it (its type parameters are the ones `decl` may
    /// mention); `None` for a base class method.
    pub iface: Option<DefId>,
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
    of: FxHashMap<DefId, usize>,
    slots: FxHashMap<(DefId, u32), usize>,
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
    ids: FxHashMap<Node, usize>,
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
        ids: FxHashMap::default(),
        nodes: vec![],
        parent: vec![],
    };
    let mut bounds: Vec<(Node, GroupBound)> = vec![];
    link_interfaces(cx, &mut uf, &mut bounds);
    let roots = link_vtables(cx, &mut uf, &mut bounds);
    let mut groups = Groups::default();
    let mut by_root: FxHashMap<usize, usize> = FxHashMap::default();
    for i in 0..uf.nodes.len() {
        let r = uf.find(i);
        let g = *by_root.entry(r).or_insert_with(|| {
            groups.list.push(Group::default());
            groups.list.len() - 1
        });
        match uf.nodes[i] {
            Node::Def(d) if cx.try_fn(d).is_some_and(|f| f.is_generator) => {}
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
        let Some(&g) = groups.of.get(&root) else {
            continue;
        };
        let g = &mut groups.list[g];
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
    let generic = b.decl.ty.is_some_and(|t| cx.mentions_params(t));
    if generic {
        if b.iface.is_some() {
            // Bounds in terms of different interfaces' parameters (`J<T> extends I<T>`) are not
            // comparable: the first one stands for the group.
            if g.bound.is_none() {
                g.bound = Some(b);
            }
            return;
        }
        cx.error(
            Diagnostic::error(
                "the `throws` clause of an overridden method cannot mention type parameters",
                b.decl.span,
            )
            .with_note("overrides are called through one vtable entry, which needs one error type"),
        );
        return;
    }
    match &g.bound {
        None => g.bound = Some(b),
        Some(old) if old.decl.ty.is_some_and(|t| cx.mentions_params(t)) => {}
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
                let iface = Some(iface);
                bounds.push((node, GroupBound { decl, owner, iface }));
            }
        }
    }
    // A method of `want` is filled by the method of that name of `have` (`upcast_impl`).
    for (have, want) in cx.synth_ifaces.upcasts.clone() {
        let (Some(h), Some(w)) = (cx.iface(have), cx.iface(want)) else {
            continue;
        };
        for (j, m) in w.methods.iter().enumerate() {
            if let Some(s) = h.methods.iter().position(|x| x.name == m.name) {
                uf.union(Node::Slot(want, j as u32), Node::Slot(have, s as u32));
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
                    let decl = cx.try_fn(m).and_then(|f| f.declared_throws);
                    if let Some(decl) = decl.or_else(|| written_rejection(cx, m)) {
                        let owner = cx.fn_info(m).name.clone();
                        let iface = None;
                        bounds.push((Node::Def(m), GroupBound { decl, owner, iface }));
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

/// The error type written in the promise a synchronous base method returns (`read():
/// Promise<u8[], IoError>`, abstract or not), as a bound of its group: what an `async` override
/// may reject with, as an interface method's promise bounds its implementations
/// (`collect::sigs::promise_throws`). A `Promise<T>` written without one is a site
/// (`promise_sites`): its overrides give it their errors.
fn written_rejection(cx: &mut Ctx, m: DefId) -> Option<DeclaredThrows> {
    let f = cx.fn_info(m);
    if f.is_async || f.is_generator || f.is_getter {
        return None;
    }
    let (ret, span) = (f.ret, f.ret_span?);
    let e = cx.ty.promise_error(ret).filter(|e| *e != cx.ty.never)?;
    if cx.site_in(span, ret).is_some() {
        return None;
    }
    Some(DeclaredThrows {
        ty: cx.canon_error(Some(e)),
        span,
        from_body: false,
    })
}

/// Bound `b` as member `m` sees it: a bound mentioning the parameters of its interface, with
/// the interface arguments of `m`'s owner (`class R implements Iterator<i64, IoError>`: `E` is
/// `IoError`; a default body or an interface extending it: in that interface's terms). `None`
/// when `m` does not throw.
pub(crate) fn member_bound(cx: &mut Ctx, b: &GroupBound, m: DefId) -> Option<TyId> {
    let t = b.decl.ty?;
    let Some(iface) = b.iface.filter(|_| cx.mentions_params(t)) else {
        return cx.canon_error(Some(t));
    };
    let owner = cx.try_fn(m).and_then(|f| f.owner);
    let args = match owner {
        Some(o) if o == iface => return cx.canon_error(Some(t)),
        Some(o) => iface_args_of(cx, o, iface, 0),
        None => None,
    };
    let args = args.or_else(|| impl_args(cx, iface, m));
    let t = match args {
        Some(args) => cx.subst(t, &args),
        None => t,
    };
    cx.canon_error(Some(t))
}

/// The arguments of `iface` as type (class, struct or interface) `d` implements or extends it,
/// in `d`'s terms: through its `implements` list, the interfaces those extend, and its base
/// classes.
fn iface_args_of(cx: &mut Ctx, d: DefId, iface: DefId, depth: u32) -> Option<Vec<TyId>> {
    if depth > 64 {
        return None;
    }
    if let Some(i) = cx.iface(d) {
        return i
            .parents
            .iter()
            .find(|p| p.iface == iface)
            .map(|p| p.args.clone());
    }
    let a = cx.adt(d)?;
    let (implements, base) = (a.implements.clone(), a.base);
    for imp in implements {
        if imp.iface == iface {
            return Some(imp.args);
        }
        if let Some(args) = iface_args_of(cx, imp.iface, iface, depth + 1) {
            return Some(args.iter().map(|t| cx.subst(*t, &imp.args)).collect());
        }
    }
    let (bd, bargs) = base.and_then(|b| cx.class_of(b))?;
    let args = iface_args_of(cx, bd, iface, depth + 1)?;
    Some(args.iter().map(|t| cx.subst(*t, &bargs)).collect())
}

/// The interface arguments of an implementation of `iface` that uses `m` (`extend` blocks).
fn impl_args(cx: &mut Ctx, iface: DefId, m: DefId) -> Option<Vec<TyId>> {
    let imp = cx.impls.iter().find(|imp| imp.methods.contains(&m))?;
    let (own, own_args) = (imp.iface, imp.iface_args.clone());
    if own == iface {
        return Some(own_args);
    }
    // A slot `iface` declares, implemented through an interface extending it (an adapter of
    // `IterableIterator` implements `Iterator`'s `next`).
    let args = iface_args_of(cx, own, iface, 0)?;
    Some(args.iter().map(|a| cx.subst(*a, &own_args)).collect())
}
