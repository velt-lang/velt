//! Which `with`, `read` and `write` callbacks never store a function value where the lock's value
//! may reach it (`hir::Program::store_free_callbacks`). When a lock is released, lowering walks
//! its value for a function value that captured what another thread may not share (velt_vir
//! glue/many.rs): every call of the function shares its captures. The value was walked when it
//! went into the lock and at every release since, and only code that holds the lock changes it,
//! so the walk can only find what the callback stored. A callback that stores no value that may
//! hold a function value (nor runs code this analysis cannot see) leaves nothing new to find, and
//! its release skips the walk.
//!
//! A body may store one when it assigns a value whose type may hold a function value
//! ([`holds_fn`]) to anything but a local of its own (a field, an element, a parameter, a
//! captured variable or a variable closures share); when it hands such a value to an intrinsic
//! or a runtime extern, which may keep it; when it calls a function value, or resumes a
//! generator, whose body is not known; and so does every body that may run one of those, along
//! the edges of module_state.rs's walk (calls, closures it makes, field initializers, the hooks
//! drops and glue run) and through the interface slots it calls (every implementation, also of
//! the interfaces whose values convert to that one). In a program that may make a `Proxy`, a
//! field access may run a trap: no callback counts.

use std::cell::RefCell;

use rustc_hash::{FxHashMap, FxHashSet};

use super::locked_props::{self, Dispatch};
use super::Facts;
use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind};
use crate::hir::{self, AdtKind, DefId, ExprKind as E, TyId, TyKind};
use crate::replaced_holders::implicit::{self, Implicit};

thread_local! {
    /// The answers of [`holds_fn`] in this pass ([`reset`]), by type and whether a type
    /// parameter counts.
    static MEMO: RefCell<FxHashMap<(TyId, bool), bool>> = RefCell::new(FxHashMap::default());
}

/// Forget the answers of [`holds_fn`]: another program's types.
pub(super) fn reset() {
    MEMO.with(|m| m.borrow_mut().clear());
}

/// What a body may store and the hooks it may run ([`Facts`]).
#[derive(Default)]
pub(super) struct StoreFacts {
    /// It may store a value that holds a function value, or run code this analysis cannot see.
    pub(super) stores: bool,
    /// It may store a value that holds a function value if a type parameter is one: left to
    /// the type arguments its callers pass.
    pub(super) param_stores: bool,
    /// The types of the values it computes and of its locals, whose `[Symbol.dispose]()` its
    /// drops may run, and of the values glue formats or copies, whose glue hooks it may run.
    pub(super) drop_tys: FxHashSet<TyId>,
    pub(super) glue_tys: Vec<TyId>,
    /// The bodies it runs with type arguments (calls, constructors, field initializers): what
    /// their type parameters stand for.
    pub(super) calls: Vec<(DefId, Vec<TyId>)>,
    /// Its other type arguments (of enum variants and of methods called through a table).
    pub(super) targs: Vec<TyId>,
}

/// The types a search looks at before it gives up, answering yes.
const BUDGET: u32 = 4096;

/// May a value of type `t` hold a function value, by itself or through its parts, as the walk
/// at a lock's release sees it (velt_vir `Cx::reaches_fn`)? A type parameter may be any type
/// (`open`) or counts for nothing; an interface value is a value of one of the program's
/// implementations (`Ctx::impls`), and a class instance one of the class or any subclass. A
/// `shared` value was walked when it was made and a promise's result is not reachable from it:
/// neither counts.
fn holds_fn(cx: &Ctx, t: TyId, open: bool) -> bool {
    match cx.ty.kind(t) {
        TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str | TyKind::Unit => {
            return false
        }
        TyKind::Closure(_) | TyKind::FnPtr { .. } => return true,
        TyKind::Param(_) => return open,
        _ => {}
    }
    if let Some(h) = MEMO.with(|m| m.borrow().get(&(t, open)).copied()) {
        return h;
    }
    let mut s = Search {
        seen: FxHashSet::default(),
        budget: BUDGET,
        subclasses: None,
    };
    let h = s.holds(cx, t, open);
    MEMO.with(|m| m.borrow_mut().insert((t, open), h));
    h
}

struct Search {
    seen: FxHashSet<(TyId, bool)>,
    budget: u32,
    /// Per class, its direct subclasses (made when first needed).
    subclasses: Option<FxHashMap<DefId, Vec<DefId>>>,
}

impl Search {
    /// `open`: a type parameter here may be any type (a generic body's, or an implementation's
    /// or a subclass's own); otherwise it is one of the object type around it, whose type
    /// arguments were looked at.
    fn holds(&mut self, cx: &Ctx, t: TyId, open: bool) -> bool {
        if !self.seen.insert((t, open)) {
            return false;
        }
        if self.budget == 0 {
            return true;
        }
        self.budget -= 1;
        if cx.dynamic(t).is_some() {
            return true;
        }
        match cx.ty.kind(t).clone() {
            TyKind::Closure(_) | TyKind::FnPtr { .. } | TyKind::Op(..) => true,
            TyKind::Param(_) => open,
            TyKind::Shared(_) | TyKind::Promise(..) => false,
            TyKind::Array(x) | TyKind::Option(x, _) => self.holds(cx, x, open),
            TyKind::Map(k, v) | TyKind::Result(k, v) => {
                self.holds(cx, k, open) || self.holds(cx, v, open)
            }
            TyKind::Tuple(xs) => xs.into_iter().any(|x| self.holds(cx, x, open)),
            TyKind::Dyn(i, args) => {
                args.into_iter().any(|a| self.holds(cx, a, open)) || self.implementations(cx, i)
            }
            TyKind::Adt(d, args) => {
                args.into_iter().any(|a| self.holds(cx, a, open)) || self.adt_holds(cx, d)
            }
            _ => false,
        }
    }

    /// Do the fields (payloads) of object type `d`, or of a subclass, hold one?
    fn adt_holds(&mut self, cx: &Ctx, d: DefId) -> bool {
        if let Some(e) = cx.enum_info(d) {
            let tys: Vec<TyId> = e.variants.iter().flat_map(|v| v.payload.clone()).collect();
            return tys.into_iter().any(|p| self.holds(cx, p, false));
        }
        let Some(a) = cx.adt(d) else { return false };
        let (class, fields): (bool, Vec<TyId>) = (
            a.kind == AdtKind::Class,
            a.fields.iter().map(|f| f.ty).collect(),
        );
        if fields.into_iter().any(|f| self.holds(cx, f, false)) {
            return true;
        }
        if !class {
            return false;
        }
        for s in self.subclasses_of(cx, d) {
            let Some(a) = cx.adt(s) else { continue };
            let fields: Vec<TyId> = a.fields.iter().map(|f| f.ty).collect();
            if fields.into_iter().any(|f| self.holds(cx, f, true)) {
                return true;
            }
            if self.adt_holds(cx, s) {
                return true;
            }
        }
        false
    }

    fn subclasses_of(&mut self, cx: &Ctx, d: DefId) -> Vec<DefId> {
        let subs = self.subclasses.get_or_insert_with(|| {
            let mut m: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
            for (i, info) in cx.info.iter().enumerate() {
                let DefInfo::Adt(a) = info else { continue };
                if let Some((b, _)) = a.base.and_then(|b| cx.class_of(b)) {
                    m.entry(b).or_default().push(DefId(i as u32));
                }
            }
            m
        });
        subs.get(&d).cloned().unwrap_or_default()
    }

    /// Does a value of an implementation of interface `i` hold one?
    fn implementations(&mut self, cx: &Ctx, i: DefId) -> bool {
        let from = converting_to(cx, i);
        let tys: Vec<TyId> = (cx.impls.iter())
            .filter(|x| from.contains(&x.iface))
            .map(|x| x.ty)
            .collect();
        tys.into_iter().any(|t| self.holds(cx, t, true))
    }
}

/// Interface `i` and the interfaces whose values the program converts to it (transitively).
fn converting_to(cx: &Ctx, i: DefId) -> Vec<DefId> {
    let mut out = vec![i];
    let mut k = 0;
    while k < out.len() {
        let to = out[k];
        for &(a, b) in &cx.synth_ifaces.upcasts {
            if b == to && !out.contains(&a) {
                out.push(a);
            }
        }
        k += 1;
    }
    out
}

/// Fill in each body's [`StoreFacts`] (`facts` holds every body and initializer node), in a
/// program with lock callbacks: a walk of its own, so a program without them pays nothing.
///
/// A callback's result, and what it throws, leave the lock through code lowering runs before
/// the release (a copy of what the value still holds, which runs its `clone()`, and drops):
/// their types' hooks count as the callback's own.
pub(super) fn collect(cx: &mut Ctx, facts: &mut FxHashMap<DefId, Facts>, callbacks: &[DefId]) {
    let nodes: Vec<DefId> = facts.keys().copied().collect();
    for d in nodes {
        let st = match cx.defs.get_mut(d.0 as usize).map(Option::take) {
            Some(Some(hir::Def::Fn(mut f))) => {
                let boxed = (f.body.locals.iter().enumerate())
                    .filter(|(_, l)| l.boxed)
                    .map(|(i, _)| i as u32);
                let mut v = StoresOf {
                    cx,
                    locals: Some((f.params.len(), boxed.collect())),
                    st: StoreFacts::default(),
                };
                // Its locals drop here, but for the parameters and captures it borrows (the
                // lock's value among them), which the caller drops.
                let borrowed = |m: hir::PassMode| {
                    matches!(m, hir::PassMode::Borrow | hir::PassMode::BorrowMut)
                };
                let lent: FxHashSet<u32> = (f.params.iter())
                    .filter(|p| borrowed(p.mode))
                    .map(|p| p.local.0)
                    .chain(
                        f.captures
                            .iter()
                            .filter(|c| borrowed(c.mode))
                            .map(|c| c.inner.0),
                    )
                    .collect();
                let owned = f.body.locals.iter().enumerate();
                let owned = owned.filter(|(i, _)| !lent.contains(&(*i as u32)));
                v.st.drop_tys.extend(owned.map(|(_, l)| l.ty));
                if callbacks.contains(&d) {
                    let out: Vec<TyId> = [f.ret].into_iter().chain(f.throws).collect();
                    v.st.drop_tys.extend(out.iter().copied());
                    v.st.glue_tys.extend(out);
                }
                crate::visit::block(&mut f.body.block, &mut v);
                let st = v.st;
                cx.defs[d.0 as usize] = Some(hir::Def::Fn(f));
                st
            }
            taken => {
                if let Some(slot) = cx.defs.get_mut(d.0 as usize) {
                    *slot = taken.flatten();
                }
                let mut v = StoresOf {
                    cx,
                    locals: None,
                    st: StoreFacts::default(),
                };
                for mut x in initializers(cx, d) {
                    crate::visit::expr(&mut x, &mut v);
                }
                v.st
            }
        };
        if let Some(f) = facts.get_mut(&d) {
            f.st = st;
        }
    }
}

/// The initializers module_state.rs scans as node `d`: a constant's, or an object type's field
/// initializers.
fn initializers(cx: &Ctx, d: DefId) -> Vec<hir::Expr> {
    match cx.info.get(d.0 as usize) {
        Some(DefInfo::Global(g)) if !g.stored => g.init.iter().cloned().collect(),
        Some(DefInfo::Adt(a)) => (a.fields.iter())
            .filter_map(|f| f.default.clone())
            .chain(a.redefined.iter().filter_map(|r| r.init.clone()))
            .collect(),
        _ => vec![],
    }
}

/// Collects a body's [`StoreFacts`].
struct StoresOf<'a, 'm> {
    cx: &'a Ctx<'m>,
    /// In a function body: its parameter count and the locals closures share.
    locals: Option<(usize, FxHashSet<u32>)>,
    st: StoreFacts,
}

impl crate::visit::VisitMut for StoresOf<'_, '_> {
    fn expr(&mut self, e: &mut hir::Expr) {
        self.note_hook_types(e);
        self.note_stores(e);
    }
}

impl StoresOf<'_, '_> {
    /// Note whether `e` may store a value that holds a function value
    /// ([`StoreFacts::stores`]).
    fn note_stores(&mut self, e: &hir::Expr) {
        if self.st.stores {
            return;
        }
        let cx = self.cx;
        // The values it may store, or `None` when it may run code this analysis cannot see.
        let stored: Option<Vec<&hir::Expr>> = match &e.kind {
            E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
                Some(match self.own_local(place) {
                    true => vec![],
                    false => vec![value],
                })
            }
            E::Call { callee, args } => match callee {
                hir::Callee::Indirect(_) => None,
                hir::Callee::Virtual { .. } => {
                    args.first().and_then(|a| cx.class_of(a.ty)).map(|_| vec![])
                }
                hir::Callee::Dyn { .. } => match args.first().map(|a| cx.ty.kind(a.ty)) {
                    Some(TyKind::Dyn(..)) => Some(vec![]),
                    _ => None,
                },
                hir::Callee::ParamMethod { .. } => Some(vec![]),
                hir::Callee::Def(f, _) => Some(match locked_props::has_body(cx, *f) {
                    true => vec![],
                    false => args.iter().collect(),
                }),
                hir::Callee::Intrinsic(hir::Intrinsic::MutexWith | hir::Intrinsic::LockRead) => {
                    match args.get(1).is_some_and(|cb| !written_fn(cb)) {
                        true => None,
                        false => Some(vec![]),
                    }
                }
                hir::Callee::Intrinsic(i) if implicit::resumes(*i) => None,
                hir::Callee::Intrinsic(i) => Some(match keeps_nothing(*i) {
                    true => vec![],
                    false => args.iter().collect(),
                }),
            },
            _ => Some(vec![]),
        };
        match stored {
            None => self.st.stores = true,
            Some(vs) => {
                for v in vs {
                    if holds_fn(cx, v.ty, false) {
                        self.st.stores = true;
                    } else if holds_fn(cx, v.ty, true) {
                        self.st.param_stores = true;
                    }
                }
            }
        }
    }

    /// The types of what `e` computes (dropped here, or by what it is handed to), of what an
    /// assignment replaces and a call is handed (which it may drop), and of what glue formats or
    /// copies (replaced_holders/implicit.rs, 1 to 3).
    fn note_hook_types(&mut self, e: &hir::Expr) {
        if !implicit::is_place(e) {
            self.st.drop_tys.insert(e.ty);
        }
        match &e.kind {
            E::Assign { place, .. } | E::CompoundAssign { place, .. } => {
                self.st.drop_tys.insert(place.ty);
            }
            // A body it calls drops what it takes (its own locals); an intrinsic or a runtime
            // extern may drop any argument.
            E::Call { callee, args } => {
                let body = match callee {
                    hir::Callee::Def(f, _) => locked_props::has_body(self.cx, *f),
                    hir::Callee::Intrinsic(_) | hir::Callee::Indirect(_) => false,
                    _ => true,
                };
                if !body {
                    self.st.drop_tys.extend(args.iter().map(|a| a.ty));
                }
            }
            _ => {}
        }
        match &e.kind {
            E::Call {
                callee: hir::Callee::Def(g, ts),
                ..
            }
            | E::FnRef(g, ts)
                if !ts.is_empty() =>
            {
                self.st.calls.push((*g, ts.clone()))
            }
            E::New { def, type_args, .. } | E::AdtLit { def, type_args, .. }
                if !type_args.is_empty() =>
            {
                let ctor = self.cx.adt(*def).and_then(|a| a.ctor);
                for g in ctor.into_iter().chain([*def]) {
                    self.st.calls.push((g, type_args.clone()));
                }
            }
            E::Variant { type_args: ts, .. }
            | E::Call {
                callee:
                    hir::Callee::Dyn {
                        method_type_args: ts,
                        ..
                    },
                ..
            } => self.st.targs.extend(ts),
            E::Call {
                callee:
                    hir::Callee::ParamMethod {
                        iface_args,
                        method_type_args,
                        ..
                    },
                ..
            } => {
                self.st.targs.extend(iface_args);
                self.st.targs.extend(method_type_args);
            }
            _ => {}
        }
        let E::Call { callee, args } = &e.kind else {
            return;
        };
        let glue = match callee {
            hir::Callee::Intrinsic(i) => implicit::glue_args(*i),
            // An async call's arguments are transferred when it is spawned.
            hir::Callee::Def(d, _) => {
                matches!(self.cx.info.get(d.0 as usize), Some(DefInfo::Fn(f)) if f.is_async)
            }
            _ => false,
        };
        if glue {
            for a in args {
                implicit::glue_types(self.cx, a, &mut self.st.glue_tys);
            }
        }
    }

    /// Is `place` a local of this body that no closure shares, not a parameter (a callback's
    /// value or a captured variable)?
    fn own_local(&self, place: &hir::Expr) -> bool {
        match (&place.kind, &self.locals) {
            (E::Local(l, _), Some((params, boxed))) => {
                l.0 as usize >= *params && !boxed.contains(&l.0)
            }
            _ => false,
        }
    }
}

/// The callbacks in `callbacks` whose bodies never store a value that holds a function value
/// (module docs): backwards from the bodies that may, along the edges and through the slots.
pub(super) fn store_free(
    cx: &Ctx,
    dispatch: &Dispatch,
    facts: &FxHashMap<DefId, Facts>,
    im: &Implicit,
    callbacks: &[DefId],
) -> FxHashSet<DefId> {
    if callbacks.is_empty() || cx.prelude_adt("Proxy").is_some() {
        return FxHashSet::default();
    }
    let cbs: FxHashSet<DefId> = callbacks.iter().copied().collect();
    let (hooks, param_stores) = by_type_args(cx, facts, im, &cbs);
    let mut callers: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
    let mut tainted: FxHashSet<DefId> = FxHashSet::default();
    let mut slot_memo: FxHashMap<(DefId, u32), Option<Vec<DefId>>> = FxHashMap::default();
    for (&d, f) in facts {
        let mut edges: Vec<DefId> = f.edges.clone();
        edges.extend(hooks.get(&d).into_iter().flatten());
        let mut unknown = f.st.stores || f.resumes || param_stores.contains(&d);
        for &(i, k) in &f.slots {
            let ms = slot_memo
                .entry((i, k))
                .or_insert_with(|| slot_methods(cx, dispatch, i, k));
            match ms {
                Some(ms) => edges.extend(ms.iter().copied()),
                None => unknown = true,
            }
        }
        for &e in &edges {
            if !facts.contains_key(&e) && runs_unseen(cx, e) {
                unknown = true;
            }
            callers.entry(e).or_default().push(d);
        }
        if unknown {
            tainted.insert(d);
        }
    }
    let mut todo: Vec<DefId> = tainted.iter().copied().collect();
    while let Some(d) = todo.pop() {
        for &c in callers.get(&d).map(Vec::as_slice).unwrap_or(&[]) {
            if tainted.insert(c) {
                todo.push(c);
            }
        }
    }
    (callbacks.iter().copied())
        .filter(|c| facts.contains_key(c) && !tainted.contains(c))
        .collect()
}

/// What type parameters make a body do, resolved by type arguments (module docs): per body, the
/// class hooks it may run (`[Symbol.dispose]()` of what it drops, glue hooks of what glue formats
/// or copies), those its types name and those the type arguments for its parameters name; and
/// the bodies that store a function value through a parameter. A generic body's parameters are
/// resolved where it is called with type arguments (a body passing its own parameters on
/// leaves them to its callers, and so on); where a generic body is reached otherwise (an
/// interface or virtual call, a closure, a hook), and in a callback, a parameter may stand for
/// any type argument the program passes, and one that may store a function value does.
fn by_type_args(
    cx: &Ctx,
    facts: &FxHashMap<DefId, Facts>,
    im: &Implicit,
    callbacks: &FxHashSet<DefId>,
) -> (FxHashMap<DefId, FxHashSet<DefId>>, FxHashSet<DefId>) {
    // A type parameter stands for any type argument the program passes, and a function value
    // may hold any value a closure captures: what they reach is `wide`, as the module-state check
    // resolves them (replaced_holders/implicit.rs `open`).
    let mut wide_args: FxHashSet<TyId> = FxHashSet::default();
    for f in facts.values() {
        wide_args.extend(f.st.calls.iter().flat_map(|c| c.1.iter().copied()));
        wide_args.extend(f.st.targs.iter().copied());
        for &t in &f.st.drop_tys {
            if let TyKind::Adt(_, args) | TyKind::Dyn(_, args) = cx.ty.kind(t) {
                wide_args.extend(args.iter().copied());
            }
        }
    }
    let wide: [Vec<DefId>; 2] = [false, true].map(|d| im.open_hooks(cx, &wide_args, d));
    // Kind 0: glue hooks, 1: dispose hooks, 2: stores. For a type: the hooks it may run (those of
    // the captures of a function value it holds included), whether a type parameter in it
    // counts, and whether it holds a function value without one.
    let res = |t: TyId, k: usize| -> (Vec<DefId>, bool, bool) {
        match k {
            2 => {
                let closed = holds_fn(cx, t, false);
                (vec![], !closed && holds_fn(cx, t, true), closed)
            }
            _ => {
                let (mut h, p, fns) = im.named_hooks(cx, t, k == 1);
                if fns {
                    h.extend(wide[k].iter().copied());
                }
                (h, p, false)
            }
        }
    };
    let mut own: FxHashMap<DefId, FxHashSet<DefId>> = FxHashMap::default();
    let mut param: FxHashMap<DefId, [bool; 3]> = FxHashMap::default();
    for (&d, f) in facts {
        let mut hooks = FxHashSet::default();
        let mut p = [false, false, f.st.param_stores];
        let glue: Vec<TyId> = f.st.glue_tys.clone();
        for (k, tys) in [glue, f.st.drop_tys.iter().copied().collect()]
            .iter()
            .enumerate()
        {
            for &t in tys {
                if im.maybe(cx, t) {
                    let (h, tp, _) = res(t, k);
                    hooks.extend(h);
                    p[k] |= tp;
                }
            }
        }
        own.insert(d, hooks);
        param.insert(d, p);
    }
    // `param`: a body leaves a type parameter's hooks or stores to its callers; `widened`: it
    // takes the program's type arguments for them (for stores: it may store).
    let mut widened: FxHashMap<DefId, [bool; 3]> = FxHashMap::default();
    loop {
        let mut changed = false;
        for (&d, f) in facts {
            let typed: FxHashSet<DefId> = f.st.calls.iter().map(|c| c.0).collect();
            for k in 0..3 {
                let mut p = param[&d][k];
                let mut w = widened.get(&d).is_some_and(|w| w[k]);
                for (g, targs) in &f.st.calls {
                    let gp = param.get(g).is_some_and(|p| p[k]);
                    p |= gp && targs.iter().any(|&t| res(t, k).1);
                    w |= k == 2 && gp && targs.iter().any(|&t| res(t, k).2);
                }
                for e in f.edges.iter().filter(|e| !typed.contains(e)) {
                    w |= param.get(e).is_some_and(|p| p[k]);
                }
                w |= p && callbacks.contains(&d);
                if p != param[&d][k] || w != widened.get(&d).is_some_and(|x| x[k]) {
                    if let Some(x) = param.get_mut(&d) {
                        x[k] = p;
                    }
                    widened.entry(d).or_default()[k] = w;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut hooks_of: FxHashMap<DefId, FxHashSet<DefId>> = FxHashMap::default();
    for (&d, f) in facts {
        let mut hooks = own.remove(&d).unwrap_or_default();
        for k in 0..2 {
            for (g, targs) in &f.st.calls {
                if param.get(g).is_some_and(|p| p[k]) {
                    hooks.extend(targs.iter().flat_map(|&t| res(t, k).0));
                }
            }
            if widened.get(&d).is_some_and(|w| w[k]) {
                hooks.extend(&wide[k]);
            }
        }
        hooks_of.insert(d, hooks);
    }
    let stores = (widened.iter())
        .filter(|(_, w)| w[2])
        .map(|(d, _)| *d)
        .collect();
    (hooks_of, stores)
}

/// The methods a call through slot `k` of interface `i` may run: those of every implementation,
/// also of the interfaces whose values convert to `i` (found by name); `None` when one is not
/// known.
fn slot_methods(cx: &Ctx, dispatch: &Dispatch, i: DefId, k: u32) -> Option<Vec<DefId>> {
    let name = cx.iface(i)?.methods.get(k as usize)?.name.clone();
    let mut out = vec![];
    for a in converting_to(cx, i) {
        let slot = cx.iface(a)?.methods.iter().position(|m| m.name == name)?;
        out.extend(dispatch.iface_methods(cx, a, slot as u32));
    }
    Some(out)
}

/// Is `d`, an edge to something the walk did not visit, code that may run unseen: a checked
/// function body (a runtime extern is not: its call is judged by its arguments; nor is a body
/// left unchecked, which no checked code reaches and lowering never builds,
/// `hir::Program::unchecked`)?
fn runs_unseen(cx: &Ctx, d: DefId) -> bool {
    matches!(cx.info.get(d.0 as usize), Some(DefInfo::Fn(f))
        if f.kind != FnKind::Extern && f.state != BodyState::Unchecked)
}

/// Is `e` a closure or a named function written in place (an optional parameter's `Some`, or
/// either branch of a conditional): a function value whose body the visit sees?
pub(super) fn written_fn(e: &hir::Expr) -> bool {
    match &e.kind {
        E::Closure(_) | E::FnRef(..) => true,
        E::WrapSome(x) | E::Upcast(x) => written_fn(x),
        E::If { then, els, .. } => written_fn(then) && written_fn(els),
        E::Block(b) if b.stmts.is_empty() => b.value.as_deref().is_some_and(written_fn),
        _ => false,
    }
}

/// Does intrinsic `i` keep none of its arguments in memory that existed before the call: it only
/// reads them, or makes a new value of them (which is stored, if at all, by an assignment or
/// another intrinsic the analysis sees), or moves elements within one array? The hooks it may
/// run (`toString()`, `clone()`) are edges of the body that calls it.
fn keeps_nothing(i: hir::Intrinsic) -> bool {
    use hir::Intrinsic as I;
    matches!(
        i,
        I::Print
            | I::PrintErr
            | I::ToString
            | I::TemplateToString
            | I::ErrorText
            | I::ThrowLoc
            | I::Inspect
            | I::StrConcat
            | I::StrLen
            | I::StrByteLen
            | I::ArrayLen
            | I::ArrayPop
            | I::ArraySwap
            | I::ArrayRemove
            | I::ArrayTruncate
            | I::ArrayPrefetch
            | I::Subarray
            | I::Hash
            | I::KeyEq
            | I::IncludesEq
            | I::Same
            | I::SameValue
            | I::Clone
            | I::Share
            | I::KeyRead
            | I::FieldAbsent
            | I::FieldPresent
            | I::NeedsTransfer
            | I::NeedsDrop
            | I::MayAlias
            | I::HoldsNull
            | I::NullAs
            | I::Truthy
            | I::IsNull
            | I::IsNullish
            | I::LooseEq
            | I::MayHoldPrimitive
            | I::HoldsDynamic
            | I::HoldsDynamicOrClass
            | I::FnCapturesNothing
            | I::SharedNew
            | I::SharedGet
            | I::SharedRead
            | I::MutexNew
            | I::RwLockNew
            | I::AnyTypeof
            | I::AnyStrictEq
    )
}
