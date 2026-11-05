//! The methods a call through an interface, a virtual method or a type parameter's method may
//! run, for [`super::changes`] (#854): the class's own method and its subclasses' overrides, or
//! the implementations of that interface method. An object type implements an interface with
//! the functions its literals store in the field of the method's name (`{ render(c) { … } }`,
//! sema's `Plugin literal` types, `object_ifaces.rs`): those are followed when every literal of the type stores a closure or a
//! named function there. When the set of methods is not known completely here (a literal
//! storing another value, a spread making the object, an implementation of another kind, an
//! `any` converted to the interface), there
//! are no targets and the caller stays conservative.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::hir::{AdtKind, Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, TyId, TyKind};
use crate::ownership::local_closures::walk::{self, Visit};

use super::values::{fits, ValueIndex};

/// A method a call may run, and how many of the call's arguments it does not take first (1
/// for a literal's function, which takes no `this`).
pub(super) type Target = (DefId, usize);

/// [`method_targets`] found so far, per kind of call, interface or class, and slot.
type TargetCache = FxHashMap<(u8, u32, u32), Option<Vec<Target>>>;

/// [`value_targets`] found so far.
type ValueCache = FxHashMap<(Vec<TyId>, Read), Option<Vec<Target>>>;

/// How a function value is read, which says what a call through it may run
/// ([`value_targets`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Read {
    /// From a place of a function type: the values made as that type.
    Typed,
    /// Of a type that is not a function type: the values that accept the arguments.
    Untyped,
    /// From an `any`: the values and bound methods that accept the arguments, whatever their
    /// number, since such a call passes what it is given.
    FromAny,
}

/// The functions the literals of each object type store in each field: `None` when one stores
/// a value that is not a closure or a named function, or a spread makes such an object.
#[derive(Default)]
pub(super) struct Literals {
    fields: FxHashMap<(DefId, u32), Option<Vec<DefId>>>,
    /// Object types a spread makes: their fields hold what was copied.
    spread: Vec<DefId>,
    /// Interfaces the program converts an `any` to: what it holds may implement them in ways
    /// not listed here.
    from_any: Vec<DefId>,
    /// The function values the program makes (closures, named functions used as values).
    values: Vec<DefId>,
    /// Each function value with the type it is made as: a function value converted to another
    /// function type is a new value, an adapter, made as that type.
    made: Vec<(DefId, TyId)>,
    /// The types the program converts to a dynamic value (`any`, `unknown`, `object`,
    /// `Function`): a function read from one may be one of their methods.
    to_any: Vec<TyId>,
    /// A host or native extern may hand the program a function, an `any` or an interface value
    /// made outside it.
    outside: bool,
    /// The program or a package stores through an `any` somewhere (`o[k] = v`, `self.out = v`):
    /// without one, nothing is fed by key (keyed.rs).
    stores_by_key: bool,
    /// [`method_targets`] per kind of call (0 bound, 1 interface, 2 virtual), the interface or
    /// class, and the slot: a virtual call's are found by a walk over every class.
    targets: std::cell::RefCell<TargetCache>,
    /// Each class's direct subclasses.
    subclasses: FxHashMap<DefId, Vec<DefId>>,
    /// [`value_targets`] per call shape (the parameters wanted, read from an `any`, operands in
    /// front), so that each is found once, not at every call of that shape.
    by_shape: std::cell::RefCell<ValueCache>,
    /// The function values by the first parameter that is not open.
    index: ValueIndex,
    /// The same, by the type each is made as, for a call through a place of a function type.
    made_as: ValueIndex,
    /// [`held_adts`], found once.
    held: std::cell::RefCell<Option<Vec<DefId>>>,
    /// The program's and packages' calls, for keyed.rs: (caller, callee) for a direct call, a
    /// constructor and a closure made; a dispatched call with its receiver's type; the
    /// functions that call through a function value or one read from an `any`; and those that
    /// store through an `any`.
    pub(super) calls: Vec<(DefId, DefId)>,
    pub(super) dispatch: Vec<(DefId, Callee, Option<TyId>)>,
    pub(super) through_values: Vec<DefId>,
    pub(super) through_any: Vec<DefId>,
    pub(super) store_sites: Vec<DefId>,
}

impl Literals {
    /// Field `k` of object type `d` gets `v`: a closure or a named function is followed, any
    /// other value makes the field's functions unknown.
    fn store(&mut self, d: DefId, k: u32, v: &Expr) {
        let f = match v.kind {
            E::Closure(c) => Some(c),
            E::FnRef(g, _) => Some(g),
            _ => None,
        };
        let entry = self.fields.entry((d, k)).or_insert_with(|| Some(vec![]));
        match (entry.as_mut(), f) {
            (Some(list), Some(g)) => list.push(g),
            _ => *entry = None,
        }
    }

    /// Scan the program's bodies (before any is taken out of `cx.defs`).
    pub(super) fn scan(cx: &Ctx) -> Literals {
        let mut out = Literals::default();
        for (i, d) in cx.defs.iter().enumerate() {
            let def = DefId(i as u32);
            // Function values std makes reach the program only through std's API; they come
            // with its declared effects (step 2), not with this join.
            let std = match cx.try_fn(def) {
                Some(info) => cx.scopes[info.module].is_std,
                None => cx.global(def).is_some_and(|g| cx.scopes[g.module].is_std),
            };
            match d {
                Some(Def::Fn(f)) => {
                    let me = (!std).then_some(def);
                    walk::block(&f.body.block, &mut Scan(cx, &mut out, std, me));
                }
                Some(Def::Global(g)) => walk::expr(&g.init, &mut Scan(cx, &mut out, std, None)),
                // A module constant's initializer is kept with its info until the end.
                _ => {
                    if let Some(init) = cx.global(def).and_then(|g| g.init.as_ref()) {
                        walk::expr(init, &mut Scan(cx, &mut out, std, None));
                    }
                    if let Some(Def::ExternFn(x)) = d {
                        out.outside |= from_outside(cx, DefId(i as u32), x);
                    }
                }
            }
        }
        out.scan_initializers(cx);
        out.index = ValueIndex::of_values(cx, &out.values);
        out.made_as = ValueIndex::of_types(cx, &out.made);
        // The classes, from sema's information: `cx.defs` holds no ADT yet at this point.
        for (i, info) in cx.info.iter().enumerate() {
            let crate::defs::DefInfo::Adt(a) = info else {
                continue;
            };
            if let Some(TyKind::Adt(b, _)) = a.base.map(|b| cx.ty.kind(b).clone()) {
                out.subclasses.entry(b).or_default().push(DefId(i as u32));
            }
        }
        out
    }

    /// Scan the initializers each class gives its own fields (`setup: F = (xs) => …`), and those
    /// it gives fields it inherits: they make function values and convert values too.
    fn scan_initializers(&mut self, cx: &Ctx) {
        for info in &cx.info {
            let crate::defs::DefInfo::Adt(a) = info else {
                continue;
            };
            let std = cx.scopes[a.module].is_std;
            let own = (a.fields.iter().skip(a.own_fields_start)).filter_map(|f| f.default.as_ref());
            let redefined = a.redefined.iter().filter_map(|r| r.init.as_ref());
            for init in own.chain(redefined) {
                walk::expr(init, &mut Scan(cx, self, std, None));
            }
        }
    }

    /// Class `d` and every class that extends it, directly or not.
    pub(super) fn below(&self, d: DefId) -> Vec<DefId> {
        let mut out = vec![];
        let mut work = vec![d];
        let mut seen = rustc_hash::FxHashSet::default();
        while let Some(c) = work.pop() {
            if seen.insert(c) {
                out.push(c);
                work.extend(self.subclasses.get(&c).into_iter().flatten().copied());
            }
        }
        out
    }

    /// The function values the program and its packages make.
    pub(super) fn values(&self) -> &[DefId] {
        &self.values
    }

    /// Is `g` a function value the program or a package makes?
    pub(super) fn is_value(&self, g: DefId) -> bool {
        self.index.has(g)
    }

    /// Does the program or a package store through an `any` anywhere?
    pub(super) fn stores_by_key(&self) -> bool {
        self.stores_by_key
    }
}

/// The scan of one definition's body; `.2`: the definition is std's; `.3`: the function whose
/// body it is, when it is the program's or a package's.
struct Scan<'a>(&'a Ctx<'a>, &'a mut Literals, bool, Option<DefId>);

impl Visit for Scan<'_> {
    fn expr(&mut self, e: &Expr) {
        if let Some(me) = self.3 {
            match &e.kind {
                E::New { def, .. } => {
                    if let Some(c) = super::reach::constructor(self.0, *def) {
                        self.1.calls.push((me, c));
                    }
                }
                E::Closure(k) | E::FnRef(k, _) => self.1.calls.push((me, *k)),
                E::Call { callee, args } => match callee {
                    Callee::Def(d, _) => self.1.calls.push((me, *d)),
                    Callee::Virtual { .. } | Callee::Dyn { .. } | Callee::ParamMethod { .. } => {
                        let recv = args.first().map(|a| a.ty);
                        self.1.dispatch.push((me, callee.clone(), recv));
                    }
                    Callee::Indirect(_) => self.1.through_values.push(me),
                    Callee::Intrinsic(Intrinsic::AnyApply) => self.1.through_any.push(me),
                    Callee::Intrinsic(Intrinsic::AnySet) => self.1.store_sites.push(me),
                    _ => {}
                },
                _ => {}
            }
        }
        match &e.kind {
            E::Closure(g) | E::FnRef(g, _) if !self.2 => {
                self.1.values.push(*g);
                self.1.made.push((*g, e.ty));
            }
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::AnySet),
                ..
            } if !self.2 => self.1.stores_by_key = true,
            E::ToDyn { expr, .. } if self.0.dynamic(e.ty).is_some() => self.1.to_any.push(expr.ty),
            E::AdtLit { def, fields, .. } => {
                if !self.0.adt(*def).is_some_and(object_type) {
                    return;
                }
                for (k, v) in fields.iter().enumerate() {
                    self.1.store(*def, k as u32, v);
                }
            }
            // A field of an object type assigned later (`obj.render = (c) => …`).
            E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
                let E::Field { base, index, .. } = &place.kind else {
                    return;
                };
                let t = match self.0.ty.kind(base.ty) {
                    TyKind::Option(inner, _) => *inner,
                    _ => base.ty,
                };
                if let TyKind::Adt(d, _) = self.0.ty.kind(t) {
                    if self.0.adt(*d).is_some_and(object_type) {
                        self.1.store(*d, *index, value);
                    }
                }
            }
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::AnyTo),
                ..
            } => {
                let t = match self.0.ty.kind(e.ty) {
                    TyKind::Option(inner, _) => *inner,
                    _ => e.ty,
                };
                if let TyKind::Dyn(iface, _) = self.0.ty.kind(t) {
                    self.1.from_any.push(*iface);
                }
            }
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::SpreadObject),
                ..
            } => {
                if let TyKind::Adt(d, _) = self.0.ty.kind(e.ty) {
                    self.1.spread.push(*d);
                }
            }
            _ => {}
        }
    }
}

/// The methods the call of `callee` with `args` (the receiver first) may run; `None` when they
/// are not known completely here.
pub(super) fn method_targets(
    cx: &Ctx,
    lits: &Literals,
    callee: &Callee,
    args: &[Expr],
) -> Option<Vec<Target>> {
    targets_for(cx, lits, callee, args.first().map(|a| a.ty))
}

/// [`method_targets`] of a call whose receiver (`args[0]`) has type `recv`.
pub(super) fn targets_for(
    cx: &Ctx,
    lits: &Literals,
    callee: &Callee,
    recv: Option<TyId>,
) -> Option<Vec<Target>> {
    let receiver = || recv.map(|t| cx.ty.kind(t).clone());
    let key = match callee {
        Callee::ParamMethod { iface, slot, .. } => Some((0, iface.0, *slot)),
        Callee::Dyn { slot, .. } => match receiver() {
            Some(TyKind::Dyn(iface, _)) => Some((1, iface.0, *slot)),
            _ => None,
        },
        Callee::Virtual { slot } => match receiver() {
            Some(TyKind::Adt(class, _)) => Some((2, class.0, *slot)),
            _ => None,
        },
        _ => None,
    };
    let key = key?;
    if let Some(t) = lits.targets.borrow().get(&key) {
        return t.clone();
    }
    let t = find_targets(cx, lits, callee, recv);
    lits.targets.borrow_mut().insert(key, t.clone());
    t
}

fn find_targets(
    cx: &Ctx,
    lits: &Literals,
    callee: &Callee,
    recv: Option<TyId>,
) -> Option<Vec<Target>> {
    let receiver = || recv.map(|t| cx.ty.kind(t).clone());
    let out = match callee {
        Callee::ParamMethod { iface, slot, .. } => implementations(cx, lits, *iface, *slot)?,
        Callee::Dyn { slot, .. } => match receiver() {
            Some(TyKind::Dyn(iface, _)) => implementations(cx, lits, iface, *slot)?,
            _ => return None,
        },
        Callee::Virtual { slot } => {
            let Some(TyKind::Adt(class, _)) = receiver() else {
                return None;
            };
            // The class and its subclasses, through the map the scan built.
            let mut out: Vec<Target> = (lits.below(class).into_iter())
                .filter_map(|c| cx.adt(c)?.vtable.get(*slot as usize).map(|&m| (m, 0)))
                .collect();
            out.sort_by_key(|t| t.0 .0);
            out.dedup();
            out
        }
        _ => return None,
    };
    (!out.is_empty()).then_some(out)
}

/// The implementations of method `slot` of interface `iface`: a class's (or struct's) method,
/// or the functions an object type's literals store in the field of that name.
fn implementations(cx: &Ctx, lits: &Literals, iface: DefId, slot: u32) -> Option<Vec<Target>> {
    if lits.from_any.contains(&iface) {
        return None;
    }
    let name = &cx.iface(iface)?.methods.get(slot as usize)?.name;
    let mut out = vec![];
    for i in cx.impls.iter().filter(|i| i.iface == iface) {
        let TyKind::Adt(d, _) = cx.ty.kind(i.ty) else {
            return None;
        };
        if !object_type(cx.adt(*d)?) {
            out.push((*i.methods.get(slot as usize)?, 0));
            continue;
        }
        out.extend(literal_fns(cx, lits, *d, name)?);
    }
    Some(out)
}

/// The functions the literals of object type `d` store in field `name`; `None` when they are
/// not known completely.
fn literal_fns(cx: &Ctx, lits: &Literals, d: DefId, name: &str) -> Option<Vec<Target>> {
    if lits.spread.contains(&d) {
        return None;
    }
    let k = cx.adt(d)?.fields.iter().position(|f| f.name == name)? as u32;
    let fns = lits.fields.get(&(d, k))?.as_ref()?;
    Some(fns.iter().map(|&g| (g, 1)).collect())
}

/// For a method `m` of an object type (the one sema makes to call the function in the field
/// of its name, which a call through an interface with one implementation calls directly):
/// `Some` of the functions it runs, `None` inside when they are not known completely.
pub(super) fn forwarded(cx: &Ctx, lits: &Literals, m: DefId) -> Option<Option<Vec<Target>>> {
    let f = cx.try_fn(m)?;
    let TyKind::Adt(d, _) = cx.ty.kind(f.this.as_ref()?.ty) else {
        return None;
    };
    if !object_type(cx.adt(*d)?) {
        return None;
    }
    let name = f.name.rsplit('.').next()?;
    Some(literal_fns(cx, lits, *d, name).filter(|t| !t.is_empty()))
}

/// Is `a` an object type (a literal's, `{ render(c) { … } }`, also one sema makes for an
/// interface it is written as), whose methods are the functions in its fields? A class or a
/// declared `struct` has methods of its own.
fn object_type(a: &crate::defs::AdtInfo) -> bool {
    a.kind == AdtKind::Anon || (a.kind == AdtKind::Struct && a.decl.is_none())
}

/// Can extern `d` hand the program a value made outside it that may be a function: a host
/// import, or a package's native function returning an `any`, a function or an interface value?
/// std's own externs return data.
fn from_outside(cx: &Ctx, d: DefId, x: &crate::hir::ExternFnDef) -> bool {
    if x.host.is_some() {
        return true;
    }
    let std = cx.try_fn(d).is_some_and(|i| cx.scopes[i.module].is_std);
    let open = cx.dynamic(x.ret).is_some()
        || matches!(cx.ty.kind(x.ret), TyKind::FnPtr { .. } | TyKind::Dyn(..));
    !std && open
}

/// The ADTs a value of an open type may be an object of when its object is not known: those the
/// program converts to a dynamic value, and the ones their fields hold, transitively (an object
/// reached through a member of a converted value: `o.inner.out = v`, `h.inner.run`).
pub(super) fn held_adts(cx: &mut Ctx, lits: &Literals) -> Vec<DefId> {
    if let Some(t) = &*lits.held.borrow() {
        return t.clone();
    }
    let mut seen: rustc_hash::FxHashSet<TyId> = rustc_hash::FxHashSet::default();
    let mut work: Vec<TyId> = lits.to_any.clone();
    let mut out = vec![];
    while let Some(t) = work.pop() {
        if !seen.insert(t) {
            continue;
        }
        match cx.ty.kind(t).clone() {
            TyKind::Adt(d, args) => {
                out.push(d);
                let below: Vec<DefId> = lits.below(d).into_iter().filter(|&c| c != d).collect();
                work.extend(super::types::adt_parts_below(cx, d, &args, &below));
            }
            k => work.extend(crate::types::children(&k)),
        }
    }
    out.sort_by_key(|d| d.0);
    out.dedup();
    *lits.held.borrow_mut() = Some(out.clone());
    out
}

/// The non-static methods of class `c`, also those no subclass overrides (not in the vtable).
pub(super) fn methods_of(cx: &Ctx, c: DefId) -> Vec<DefId> {
    let mut methods: Vec<DefId> = (cx.adt(c).into_iter())
        .flat_map(|a| a.methods.values().filter(|m| !m.is_static).map(|m| m.def))
        .collect();
    if let Some(a) = cx.adt(c) {
        methods.extend(a.vtable.iter().copied());
    }
    methods.sort_by_key(|m| m.0);
    methods
}

/// The functions a call through a function value may run (#854; write-effects.md "Function
/// values"): those [`Read`] says, each of a type that accepts the call's (`want`: the function
/// type's parameters, or for a function read from an `any` the arguments' types before they were
/// converted). A method read from an `any` is one of the classes the program converts to `any`
/// or of the classes those hold ([`held_adts`]), bound to its object. A target's count is the
/// call's operands in front of the arguments it takes: `skip`, one less for a bound method.
/// `None` when a value made outside the program may be called.
pub(super) fn value_targets(
    cx: &mut Ctx,
    lits: &Literals,
    want: &[TyId],
    read: Read,
    skip: usize,
) -> Option<Vec<Target>> {
    if lits.outside {
        return None;
    }
    let key = (want.to_vec(), read);
    if let Some(t) = lits.by_shape.borrow().get(&key) {
        return Some(t.clone()?.into_iter().map(|(g, s)| (g, skip - s)).collect());
    }
    // The classes a value read from an `any` may be an object of: those converted, and what
    // they hold (`h.inner.run` of a converted `Holder`).
    let held = match read {
        Read::FromAny => held_adts(cx, lits),
        _ => vec![],
    };
    let t = find_values(cx, lits, want, &held, read);
    lits.by_shape.borrow_mut().insert(key, t.clone());
    Some(t?.into_iter().map(|(g, s)| (g, skip - s)).collect())
}

/// The targets of [`value_targets`], each with 1 for a bound method (it takes one more of the
/// call's operands: its `this` stands where a value's call has an operand of its own), else 0.
fn find_values(
    cx: &Ctx,
    lits: &Literals,
    want: &[TyId],
    held: &[DefId],
    read: Read,
) -> Option<Vec<Target>> {
    let index = match read {
        Read::Typed => &lits.made_as,
        _ => &lits.index,
    };
    let (exact, loose) = (read == Read::Typed, read == Read::FromAny);
    let candidates = index.candidates(cx, want, loose);
    crate::check_work::add_keyed_work(candidates.len() as u64 + 1);
    let mut out: Vec<Target> = vec![];
    let mut seen = rustc_hash::FxHashSet::default();
    for i in candidates {
        if fits(cx, index.params(i), want, exact, loose) && seen.insert(index.value(i)) {
            out.push((index.value(i), 0));
        }
    }
    seen.clear();
    for &c in held {
        for m in methods_of(cx, c) {
            let Some(Def::Fn(f)) = &cx.defs[m.0 as usize] else {
                continue;
            };
            let new = seen.insert(m) && !lits.index.has(m);
            let params: Vec<TyId> = f.params.iter().skip(1).map(|p| p.ty).collect();
            if new && !f.params.is_empty() && fits(cx, &params, want, false, loose) {
                out.push((m, 1));
            }
        }
    }
    Some(out)
}
