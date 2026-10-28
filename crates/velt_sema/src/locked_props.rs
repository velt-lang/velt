//! Which `with`, `read` and `write` callbacks may reach a function's properties held as `any`
//! (#924; `hir::Program::lock_free_callbacks`). A declared function's properties are module
//! state the runtime keeps (velt_rt fn_props.rs), and a callback cannot wait for the module lock
//! while it holds a `Mutex`: lowering takes the module lock before the lock of every call whose
//! callback may reach them (velt_rt `velt_rt_module_with`). This finds the others, which never
//! do, so their calls cost what they cost before.
//!
//! A body may reach a function's properties when it reads, writes, deletes, tests or lists a
//! member of a value that is or holds `any` / `unknown` (an intrinsic, or a runtime extern, with
//! such an argument: `Object.keys`, `JSON.stringify`, printing), handles a type a `Proxy` may be
//! behind ([`Proxies`]), calls a function value whose body is not known, or runs a `with` whose
//! callback is held in a value (lowering takes the module lock for that one); and so does every
//! body that may run one of those, along the edges of module_state.rs's walk (calls, closures it
//! makes, hooks). A callback containing a `with` whose callback may reach them is one too,
//! through the edge to that callback: the inner call never waits for the module lock while the
//! outer `Mutex` is held.

use std::cell::RefCell;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::defs::{BodyState, FnKind};
use crate::hir::{self, DefId, ExprKind as E, TyId, TyKind};

thread_local! {
    /// The answers of [`holds_dynamic`] in this pass ([`reset`]).
    static MEMO: RefCell<FxHashMap<TyId, bool>> = RefCell::new(FxHashMap::default());
}

/// Forget the answers of [`holds_dynamic`]: another program's types.
pub(crate) fn reset() {
    MEMO.with(|m| m.borrow_mut().clear());
}

/// Does a value of type `t` hold an `any` / `unknown` by itself or through its parts? A type
/// parameter does not count: the runtime reaches a function's properties only through a value
/// typed `any` (generic code converts a `T` to `any` before it lists or prints it), and a generic
/// standard library body is analysed once for every `T` (a promise's settlement).
pub(crate) fn holds_dynamic(cx: &Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str | TyKind::Unit => {
            return false;
        }
        TyKind::Closure(_) | TyKind::FnPtr { .. } | TyKind::Literal(_) | TyKind::Param(_) => {
            return false;
        }
        _ => {}
    }
    if let Some(h) = MEMO.with(|m| m.borrow().get(&t).copied()) {
        return h;
    }
    let mut seen = FxHashSet::default();
    let h = holds(cx, t, &mut seen);
    MEMO.with(|m| m.borrow_mut().insert(t, h));
    h
}

fn holds(cx: &Ctx, t: TyId, seen: &mut FxHashSet<TyId>) -> bool {
    if !seen.insert(t) {
        return false;
    }
    if cx.dynamic(t).is_some() {
        return true;
    }
    match cx.ty.kind(t).clone() {
        TyKind::Array(x) | TyKind::Option(x, _) | TyKind::Shared(x) | TyKind::Promise(x, _) => {
            holds(cx, x, seen)
        }
        TyKind::Map(k, v) | TyKind::Result(k, v) => holds(cx, k, seen) || holds(cx, v, seen),
        TyKind::Tuple(xs) => xs.iter().any(|&x| holds(cx, x, seen)),
        TyKind::Adt(d, args) => {
            if args.iter().any(|&a| holds(cx, a, seen)) {
                return true;
            }
            let fields: Vec<TyId> = cx
                .adt(d)
                .map_or(vec![], |a| a.fields.iter().map(|f| f.ty).collect());
            fields.into_iter().any(|f| holds(cx, f, seen))
        }
        // An interface value may be an object of any class implementing it.
        TyKind::Dyn(..) => true,
        _ => false,
    }
}

/// A node of the walk: a body, or an interface slot standing for its implementations.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    Body(DefId),
    Slot(DefId, u32),
}

/// What each body does: whether it reaches a function's properties by itself, what it may run,
/// and the interface slots it calls through.
pub(crate) type Graph<'f> = FxHashMap<DefId, (bool, &'f [DefId], &'f [(DefId, u32)])>;

/// The callbacks in `callbacks` whose bodies never reach a function's properties (backwards
/// from the bodies that do, along the edges and through the slots).
pub(crate) fn lock_free(
    cx: &Ctx,
    dispatch: &Dispatch,
    graph: &Graph,
    callbacks: &[DefId],
) -> FxHashSet<DefId> {
    let mut callers: FxHashMap<Node, Vec<Node>> = FxHashMap::default();
    let mut slots: FxHashSet<(DefId, u32)> = FxHashSet::default();
    let mut reach: FxHashSet<Node> = FxHashSet::default();
    let mut todo = vec![];
    for (&d, &(props, edges, called)) in graph {
        for &e in edges {
            callers
                .entry(Node::Body(e))
                .or_default()
                .push(Node::Body(d));
        }
        for &(i, k) in called {
            callers
                .entry(Node::Slot(i, k))
                .or_default()
                .push(Node::Body(d));
            slots.insert((i, k));
        }
        if props && reach.insert(Node::Body(d)) {
            todo.push(Node::Body(d));
        }
    }
    for (i, k) in slots {
        for m in dispatch.iface_methods(cx, i, k) {
            callers
                .entry(Node::Body(m))
                .or_default()
                .push(Node::Slot(i, k));
        }
    }
    while let Some(d) = todo.pop() {
        for &c in callers.get(&d).map(Vec::as_slice).unwrap_or(&[]) {
            if reach.insert(c) {
                todo.push(c);
            }
        }
    }
    (callbacks.iter().copied())
        .filter(|c| graph.contains_key(c) && !reach.contains(&Node::Body(*c)))
        .collect()
}

/// The methods calls through a table may run, indexed once per pass (module_state.rs follows
/// them for every call): per class and slot, the class's own method and every subclass's; per
/// interface, its implementations.
pub(crate) struct Dispatch {
    virtuals: FxHashMap<(DefId, u32), Vec<DefId>>,
    impls: FxHashMap<DefId, Vec<usize>>,
}

impl Dispatch {
    pub(crate) fn new(cx: &Ctx) -> Dispatch {
        let mut virtuals: FxHashMap<(DefId, u32), Vec<DefId>> = FxHashMap::default();
        for (i, info) in cx.info.iter().enumerate() {
            let DefInfo::Adt(a) = info else { continue };
            for (slot, &m) in a.vtable.iter().enumerate() {
                let mut cur = Some(DefId(i as u32));
                let mut depth = 0;
                while let Some(c) = cur {
                    virtuals.entry((c, slot as u32)).or_default().push(m);
                    depth += 1;
                    cur = match cx.adt(c).and_then(|a| a.base) {
                        Some(b) if depth < 64 => cx.class_of(b).map(|(d, _)| d),
                        _ => None,
                    };
                }
            }
        }
        let mut impls: FxHashMap<DefId, Vec<usize>> = FxHashMap::default();
        for (i, imp) in cx.impls.iter().enumerate() {
            impls.entry(imp.iface).or_default().push(i);
        }
        Dispatch { virtuals, impls }
    }

    pub(crate) fn virtual_methods(&self, class: DefId, slot: u32) -> Vec<DefId> {
        self.virtuals
            .get(&(class, slot))
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn iface_methods(&self, cx: &Ctx, iface: DefId, slot: u32) -> Vec<DefId> {
        let imps = self.impls.get(&iface).map(Vec::as_slice).unwrap_or(&[]);
        (imps.iter())
            .filter_map(|&i| cx.impls[i].methods.get(slot as usize).copied())
            .collect()
    }
}

/// Does the walk follow `f`'s body (a function with one, not a runtime extern)?
pub(crate) fn has_body(cx: &Ctx, f: DefId) -> bool {
    matches!(cx.info.get(f.0 as usize), Some(DefInfo::Fn(i))
        if i.kind != FnKind::Extern && i.state == BodyState::Done)
}

/// What makes a typed value a proxy (velt_vir `proxy_typed.rs`): in a program that loads
/// `Proxy`, an `any` converted to an object type (`AnyTo`, in the program's own code) may hold
/// one, and reading or writing a field of it runs a trap, which sema sees as a field access.
/// So a body handling such a type, or one a value of it is upcast to, may reach a function's
/// properties.
#[derive(Default)]
pub(crate) struct Proxies {
    targets: Vec<TyId>,
    casts: Vec<(TyId, TyId)>,
}

impl Proxies {
    /// Note what `e` (in the program's own code) adds.
    pub(crate) fn note(&mut self, e: &hir::Expr) {
        match &e.kind {
            E::Call {
                callee: hir::Callee::Intrinsic(hir::Intrinsic::AnyTo),
                ..
            } => self.targets.push(e.ty),
            E::Upcast(x) if x.ty != e.ty => self.casts.push((x.ty, e.ty)),
            _ => {}
        }
    }

    pub(crate) fn add(&mut self, o: Proxies) {
        self.targets.extend(o.targets);
        self.casts.extend(o.casts);
    }

    /// Mark the bodies (their types and whether they reach a function's properties) that handle
    /// a type that may hold a proxy.
    pub(crate) fn reach<'f>(
        &self,
        cx: &Ctx,
        bodies: impl Iterator<Item = (&'f FxHashSet<TyId>, &'f mut bool)>,
    ) {
        // A program that loads `Proxy` may make one.
        if cx.prelude_adt("Proxy").is_none() {
            return;
        }
        let mut p = Proxiable::default();
        for &t in &self.targets {
            p.add(cx, t);
        }
        loop {
            let before = (p.roots.len(), p.types.len(), p.all);
            for &(from, to) in &self.casts {
                if p.holds(cx, from) {
                    p.add(cx, to);
                }
            }
            if (p.roots.len(), p.types.len(), p.all) == before {
                break;
            }
        }
        for (tys, props) in bodies {
            if !*props && tys.iter().any(|&t| p.holds(cx, t)) {
                *props = true;
            }
        }
    }
}

/// The types that may hold a proxy: class hierarchies (by root) and other object types; `all`
/// when a conversion's target is generic (any object type, then).
#[derive(Default)]
struct Proxiable {
    roots: FxHashSet<DefId>,
    types: FxHashSet<TyId>,
    all: bool,
}

impl Proxiable {
    fn add(&mut self, cx: &Ctx, t: TyId) {
        match cx.ty.kind(t) {
            TyKind::Option(x, _) | TyKind::Array(x) => self.add(cx, *x),
            TyKind::Param(_) | TyKind::Op(..) => self.all = true,
            TyKind::Adt(_, args) => {
                if args
                    .iter()
                    .any(|&a| matches!(cx.ty.kind(a), TyKind::Param(_)))
                {
                    self.all = true;
                }
                match cx.class_of(t) {
                    Some((d, _)) => {
                        self.roots.insert(root(cx, d));
                    }
                    None => {
                        self.types.insert(t);
                    }
                }
            }
            _ => {}
        }
    }

    fn holds(&self, cx: &Ctx, t: TyId) -> bool {
        match cx.ty.kind(t) {
            TyKind::Option(x, _) | TyKind::Array(x) => self.holds(cx, *x),
            TyKind::Adt(..) if self.all => true,
            TyKind::Adt(..) => match cx.class_of(t) {
                Some((d, _)) => self.roots.contains(&root(cx, d)),
                None => self.types.contains(&t),
            },
            _ => false,
        }
    }
}

/// The root of the class hierarchy `d` is in.
fn root(cx: &Ctx, mut d: DefId) -> DefId {
    for _ in 0..64 {
        match cx.adt(d).and_then(|a| a.base).and_then(|b| cx.class_of(b)) {
            Some((b, _)) => d = b,
            None => break,
        }
    }
    d
}
