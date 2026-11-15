//! Calls through a function value read out of an object's field (`arms.ready(v)`): the callee
//! is one of the values stored into that field, so a closure that crosses reaches such a call
//! only if it may be stored there (#2808). Matching by type alone, a type parameter in the
//! field's type (`(v: T) => R`) matched every closure, the prelude's `resolve` included.
//!
//! A *slot* is a field of an object type (`AdtDef`), across all its instantiations. Its stores are
//! the object literals' fields and the assignments `x.f = v` the graph walks; what each stored
//! value may be is traced back along the graph's edges to closure literals and named functions.
//! Anything else makes the slot *open*, matched by type as before:
//! - a stored value of unknown origin (a read, a call's result, a parameter nothing passes);
//! - a type whose fields the walk can't see written: a class with a base class or subclasses
//!   (a read through the base sees the subclass's stores), one converted to or from another type
//!   (`any`, an interface, a cast), one indexed by a computed key, or one built or assigned by code
//!   outside every function body (module-level and field initializers, which the graph doesn't
//!   walk);
//! - a field with an initializer other than a literal (a class field's `= () => …`).

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::ownership::local_closures::walk::{self, Visit};

use super::graph::{Graph, Node};

/// A field of an object type: the type's definition and the field's index.
pub(super) type Slot = (DefId, u32);

/// What the walk saw stored into slots.
#[derive(Default)]
pub(super) struct Slots {
    /// Per slot, the nodes and named functions stored into it.
    stores: FxHashMap<Slot, Vec<Stored>>,
    /// Slots written by something the trace can't follow.
    open: FxHashSet<Slot>,
    /// Types whose fields may be written where the walk doesn't see it.
    open_types: FxHashSet<DefId>,
    /// Conversions of a type that mentions a type parameter, with the function making them:
    /// what they open is what the type is in the function's instantiations.
    generic: Vec<(DefId, TyId)>,
}

/// The object types the generic conversions open ([`Slots::generic_open`]); `None`: every one.
pub(super) type GenericOpen = Option<FxHashSet<DefId>>;

#[derive(Clone, Copy)]
pub(super) enum Stored {
    Node(usize),
    Fn(DefId),
}

/// The slot `f` reads, when the callee of a call through a function value is a field of an
/// object (`arms.ready`, `arms.pending?.()`).
pub(super) fn callee_slot(cx: &Ctx, mut f: &Expr) -> Option<Slot> {
    loop {
        match &f.kind {
            E::UnwrapSome(x, _) => f = x,
            E::Field { base, index, .. } => return adt_of(cx, base).map(|d| (d, *index)),
            _ => return None,
        }
    }
}

fn adt_of(cx: &Ctx, e: &Expr) -> Option<DefId> {
    match cx.ty.kind(e.ty) {
        TyKind::Adt(d, _) => Some(*d),
        _ => None,
    }
}

impl Slots {
    /// What [`classify`] found stored into field `index` of an object of type `def`.
    pub(super) fn store(&mut self, def: DefId, index: u32, what: Option<Option<Stored>>) {
        let slot = (def, index);
        match what {
            Some(Some(s)) => self.stores.entry(slot).or_default().push(s),
            Some(None) => {}
            None => _ = self.open.insert(slot),
        }
    }

    /// A value of type `e.ty` is converted (a cast, an upcast, to an interface or `any`, an
    /// intrinsic) or written by a computed key, in function `d`: the walk can't see what is stored
    /// into the fields of the objects it holds from there. Every object type it mentions is open;
    /// a type parameter's are those of `d`'s instantiations ([`Slots::generic_open`]).
    pub(super) fn open_type(&mut self, cx: &Ctx, d: DefId, e: &Expr) {
        let mut generic = false;
        cx.ty.visit(e.ty, &mut |k| match k {
            TyKind::Adt(a, _) => _ = self.open_types.insert(*a),
            TyKind::Param(_) => generic = true,
            _ => {}
        });
        if generic {
            self.generic.push((d, e.ty));
        }
    }

    /// The object types the generic conversions open: those their types become in their
    /// functions' instantiations, or `None` (all) when one stays generic.
    pub(super) fn generic_open(&self, cx: &mut Ctx, g: &Graph) -> GenericOpen {
        let mut out = FxHashSet::default();
        for &(d, ty) in &self.generic {
            for t in super::instances(cx, g, d, ty) {
                let mut generic = false;
                cx.ty.visit(t, &mut |k| match k {
                    TyKind::Adt(a, _) => _ = out.insert(*a),
                    TyKind::Param(_) => generic = true,
                    _ => {}
                });
                if generic {
                    return None;
                }
            }
        }
        Some(out)
    }

    /// The closures and named functions slot `s` may hold, or `None` when it is open. A value is
    /// followed back only through nodes whose every write the graph records ([`complete`]).
    pub(super) fn holders(
        &self,
        cx: &Ctx,
        g: &Graph,
        s: Slot,
        generic: &GenericOpen,
        written: &FxHashSet<usize>,
    ) -> Option<FxHashSet<DefId>> {
        if self.open.contains(&s) || self.open_types.contains(&s.0) {
            return None;
        }
        if generic.as_ref().is_none_or(|o| o.contains(&s.0)) {
            return None;
        }
        let mut out = FxHashSet::default();
        let mut seen = FxHashSet::default();
        let mut work = vec![];
        for st in self.stores.get(&s).into_iter().flatten() {
            match *st {
                Stored::Fn(d) => _ = out.insert(d),
                Stored::Node(n) => work.push(n),
            }
        }
        while let Some(n) = work.pop() {
            if !seen.insert(n) {
                continue;
            }
            if let Node::Lit(c) = g.nodes[n] {
                out.insert(c);
                continue;
            }
            if !complete(cx, g, n, written) {
                return None;
            }
            work.extend(g.srcs[n].iter().map(|&(m, _)| m));
        }
        Some(out)
    }

    /// The types and fields that code the walk doesn't see may write: classes in a hierarchy,
    /// fields with initializers, and objects built or assigned outside every function body.
    pub(super) fn finish(&mut self, cx: &Ctx) {
        let mut outside = Outside(&mut self.open_types, cx);
        for (i, info) in cx.info.iter().enumerate() {
            match info {
                DefInfo::Global(gi) => {
                    if let Some(init) = &gi.init {
                        walk::expr(init, &mut outside);
                    }
                }
                DefInfo::Adt(a) => {
                    for v in a.redefined.iter().filter_map(|r| r.init.as_ref()) {
                        walk::expr(v, &mut outside);
                    }
                    for (k, f) in a.fields.iter().enumerate() {
                        if let Some(v) = &f.default {
                            walk::expr(v, &mut outside);
                            if !matches!(v.kind, E::Lit(_)) {
                                self.open.insert((DefId(i as u32), k as u32));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for (i, def) in cx.defs.iter().enumerate() {
            match def {
                Some(Def::Global(gd)) => walk::expr(&gd.init, &mut outside),
                Some(Def::Adt(a)) => {
                    if let Some(b) = a.base {
                        outside.0.insert(DefId(i as u32));
                        if let TyKind::Adt(bd, _) = cx.ty.kind(b) {
                            outside.0.insert(*bd);
                        }
                    }
                    for (k, f) in a.fields.iter().enumerate() {
                        if let Some(v) = &f.default {
                            walk::expr(v, &mut outside);
                            if !matches!(v.kind, E::Lit(_)) {
                                self.open.insert((DefId(i as u32), k as u32));
                            }
                        }
                    }
                    for init in &a.inherited_inits {
                        walk::expr(&init.init, &mut outside);
                    }
                }
                _ => {}
            }
        }
        // A base class's own bases (only one level was marked above).
        let mut changed = true;
        while changed {
            changed = false;
            for (i, def) in cx.defs.iter().enumerate() {
                if let (Some(Def::Adt(a)), true) = (def, self.open_types.contains(&DefId(i as u32)))
                {
                    if let Some(TyKind::Adt(bd, _)) = a.base.map(|b| cx.ty.kind(b)) {
                        changed |= self.open_types.insert(*bd);
                    }
                }
            }
        }
    }
}

/// What a stored expression may hold: `Some(Some(_))` a node or a named function to follow,
/// `Some(None)` no function (a literal), `None` something the trace can't follow. `node` gives
/// the node of a local or a closure literal.
pub(super) fn classify(
    e: &Expr,
    node: &mut impl FnMut(&Expr) -> Option<usize>,
) -> Option<Option<Stored>> {
    match &e.kind {
        E::Lit(_) => Some(None),
        E::FnRef(d, _) => Some(Some(Stored::Fn(*d))),
        E::WrapSome(x) | E::UnwrapSome(x, _) => classify(x, node),
        _ => node(e).map(|n| Some(Stored::Node(n))),
    }
}

/// Marks the types of the objects code outside every function body builds or assigns.
struct Outside<'a, 'c, 'm>(&'a mut FxHashSet<DefId>, &'c Ctx<'m>);

impl Visit for Outside<'_, '_, '_> {
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::AdtLit { def, .. } => _ = self.0.insert(*def),
            E::Assign { place, .. } => {
                if let E::Field { base, .. } | E::Index { base, .. } = &place.kind {
                    self.0.extend(adt_of(self.1, base));
                }
            }
            _ => {}
        }
    }
}

/// Does the graph record every value written into node `n`? Not for a value of unknown origin, a
/// node with no inflow (a parameter nothing the graph follows passes), a variable a closure
/// assigns (`written`: the graph links a captured variable into the closure, not back), or a
/// parameter of a function some caller reaches without the graph following its arguments (a
/// function used as a value, a method reached by dispatch, a closure).
fn complete(cx: &Ctx, g: &Graph, n: usize, written: &FxHashSet<usize>) -> bool {
    if g.unknown[n] || g.srcs[n].is_empty() || written.contains(&n) {
        return false;
    }
    match g.nodes[n] {
        Node::Local(d, l) => match &cx.defs[d.0 as usize] {
            Some(Def::Fn(f)) => {
                let param = f.params.iter().skip(f.captures.len()).any(|p| p.local == l);
                !param || super::call_sites::results_followed(cx, g, d)
            }
            _ => true,
        },
        _ => true,
    }
}

/// The variables a closure assigns: for each closure, a captured variable whose capture local
/// gets a value from inside the closure, and, through closures created in closures, the variables
/// those capture in turn.
pub(super) fn closure_written(cx: &Ctx, g: &Graph) -> FxHashSet<usize> {
    let mut out = FxHashSet::default();
    let mut work = vec![];
    for (i, def) in cx.defs.iter().enumerate() {
        let Some(Def::Fn(f)) = def else { continue };
        let c = DefId(i as u32);
        let Some(&parent) = g.parent.get(&c) else {
            continue;
        };
        for cap in &f.captures {
            let (Some(&inner), Some(&outer)) = (
                g.ids.get(&Node::Local(c, cap.inner)),
                g.ids.get(&Node::Local(parent, cap.outer)),
            ) else {
                continue;
            };
            // A value from inside the closure: an edge from another node, or one the graph doesn't
            // follow (a named function, a call's result), which marks the local unknown.
            let assigned = g.unknown[inner] || g.srcs[inner].iter().any(|&(m, _)| m != outer);
            if assigned || out.contains(&inner) {
                work.push(outer);
            }
        }
    }
    // A capture local opened above is itself a captured variable of its closure's parent.
    let caps: FxHashMap<usize, usize> = (cx.defs.iter().enumerate())
        .filter_map(|(i, d)| match d {
            Some(Def::Fn(f)) => Some((DefId(i as u32), f)),
            _ => None,
        })
        .filter_map(|(c, f)| g.parent.get(&c).map(|&p| (c, p, f)))
        .flat_map(|(c, p, f)| {
            f.captures.iter().filter_map(move |cap| {
                Some((
                    *g.ids.get(&Node::Local(c, cap.inner))?,
                    *g.ids.get(&Node::Local(p, cap.outer))?,
                ))
            })
        })
        .collect();
    while let Some(n) = work.pop() {
        if out.insert(n) {
            if let Some(&o) = caps.get(&n) {
                work.push(o);
            }
        }
    }
    out
}

/// An intrinsic that moves a value into or out of `any` (the `Any*` family, `ToAny`) or copies an
/// object's fields into another object (`SpreadObject`).
pub(super) fn converts(i: crate::hir::Intrinsic) -> bool {
    use crate::hir::Intrinsic as I;
    matches!(i, I::ToAny | I::SpreadObject) || format!("{i:?}").starts_with("Any")
}
