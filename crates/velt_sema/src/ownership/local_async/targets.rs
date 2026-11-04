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

/// A method a call may run, and how many of the call's arguments it does not take first (1
/// for a literal's function, which takes no `this`).
pub(super) type Target = (DefId, usize);

/// [`method_targets`] found so far, per kind of call, interface or class, and slot.
type TargetCache = FxHashMap<(u8, u32, u32), Option<Vec<Target>>>;

/// [`value_targets`] found so far.
type ValueCache = FxHashMap<(Vec<TyId>, bool, usize), Option<Vec<Target>>>;

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
    /// [`held_adts`], found once.
    held: std::cell::RefCell<Option<Vec<DefId>>>,
    /// The program's and packages' calls, for keyed.rs: (caller, callee) for a direct call, a
    /// constructor and a closure made; a dispatched call with its receiver's type; the
    /// functions that call through a function value; and those that store through an `any`.
    pub(super) calls: Vec<(DefId, DefId)>,
    pub(super) dispatch: Vec<(DefId, Callee, Option<TyId>)>,
    pub(super) through_values: Vec<DefId>,
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
        out.index = ValueIndex::new(cx, &out.values);
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
                E::Closure(k) => self.1.calls.push((me, *k)),
                E::Call { callee, args } => match callee {
                    Callee::Def(d, _) => self.1.calls.push((me, *d)),
                    Callee::Virtual { .. } | Callee::Dyn { .. } | Callee::ParamMethod { .. } => {
                        let recv = args.first().map(|a| a.ty);
                        self.1.dispatch.push((me, callee.clone(), recv));
                    }
                    Callee::Indirect(_) => self.1.through_values.push(me),
                    Callee::Intrinsic(Intrinsic::AnySet) => self.1.store_sites.push(me),
                    _ => {}
                },
                _ => {}
            }
        }
        match &e.kind {
            E::Closure(g) | E::FnRef(g, _) if !self.2 => self.1.values.push(*g),
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

/// Is class `d` class `base` or one of its subclasses?
fn descends(cx: &Ctx, d: DefId, base: DefId) -> bool {
    let mut d = d;
    for _ in 0..64 {
        if d == base {
            return true;
        }
        let Some(TyKind::Adt(b, _)) = cx.adt(d).and_then(|a| a.base).map(|b| cx.ty.kind(b)) else {
            return false;
        };
        d = *b;
    }
    false
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

/// The functions a call through a function value may run (#854; write-effects.md "Function
/// values"): the program's function values that take as many arguments, each of a type that
/// accepts the call's (`want`: the function type's parameters, or for a function read from an
/// `any` the arguments' types before they were converted). For one read from an `any`
/// (`from_any`), also the methods of the classes the program converts to `any` and of the
/// classes those hold ([`held_adts`]), bound to their object. `skip`: the call's operands in front of the arguments. `None` when a value made
/// outside the program may be called.
pub(super) fn value_targets(
    cx: &mut Ctx,
    lits: &Literals,
    want: &[TyId],
    from_any: bool,
    skip: usize,
) -> Option<Vec<Target>> {
    if lits.outside {
        return None;
    }
    let key = (want.to_vec(), from_any, skip);
    if let Some(t) = lits.by_shape.borrow().get(&key) {
        return t.clone();
    }
    // The classes a value read from an `any` may be an object of: those converted, and what
    // they hold (`h.inner.run` of a converted `Holder`).
    let held = match from_any {
        true => held_adts(cx, lits),
        false => vec![],
    };
    let t = find_values(cx, lits, want, &held, skip);
    lits.by_shape.borrow_mut().insert(key, t.clone());
    t
}

fn find_values(
    cx: &Ctx,
    lits: &Literals,
    want: &[TyId],
    held: &[DefId],
    skip: usize,
) -> Option<Vec<Target>> {
    let mut out: Vec<Target> = vec![];
    let fits = |ps: &[crate::hir::Param]| {
        ps.len() == want.len() && ps.iter().zip(want).all(|(p, &w)| accepts(cx, p.ty, w))
    };
    let candidates = lits.index.candidates(cx, want);
    crate::check_work::add_keyed_work(candidates.len() as u64 + 1);
    for g in candidates {
        let Some(Def::Fn(f)) = &cx.defs[g.0 as usize] else {
            continue;
        };
        if fits(&f.params[f.captures.len()..]) {
            out.push((g, skip));
        }
    }
    let mut seen = rustc_hash::FxHashSet::default();
    {
        for c in held {
            // Its methods, also those no subclass overrides (not in the vtable).
            let mut methods: Vec<DefId> = (cx.adt(*c).into_iter())
                .flat_map(|a| a.methods.values().filter(|m| !m.is_static).map(|m| m.def))
                .collect();
            if let Some(a) = cx.adt(*c) {
                methods.extend(a.vtable.iter().copied());
            }
            methods.sort_by_key(|m| m.0);
            for m in methods {
                let Some(Def::Fn(f)) = &cx.defs[m.0 as usize] else {
                    continue;
                };
                // A bound method: its `this` is the object, not one of the call's operands.
                let new = seen.insert(m) && !lits.index.has(m);
                if new && !f.params.is_empty() && fits(&f.params[1..]) {
                    out.push((m, skip - 1));
                }
            }
        }
    }
    Some(out)
}

/// What a parameter's type must be for a value of a type to be possibly accepted, without looking
/// further than its outermost type ([`unify`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Head {
    Adt(DefId),
    Dyn(DefId),
    Kind(std::mem::Discriminant<TyKind>),
}

/// A type without its `| null`.
fn strip(cx: &Ctx, t: TyId) -> TyId {
    match cx.ty.kind(t) {
        TyKind::Option(inner, _) => *inner,
        _ => t,
    }
}

/// A type that unifies with every other: a type parameter, or a dynamic type.
fn is_open(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Param(_)) || cx.dynamic(t).is_some()
}

fn head(cx: &Ctx, t: TyId) -> Head {
    match cx.ty.kind(t) {
        TyKind::Adt(d, _) => Head::Adt(*d),
        TyKind::Dyn(d, _) => Head::Dyn(*d),
        k => Head::Kind(std::mem::discriminant(k)),
    }
}

/// The program's function values of one arity. Each is listed once, under its first parameter
/// that is not open (or in `open`), by its position in [`Literals`]' values.
#[derive(Default)]
struct Arity {
    open: Vec<u32>,
    at: Vec<Position>,
}

/// The function values of an arity listed under a parameter position.
#[derive(Default)]
struct Position {
    all: Vec<u32>,
    by_head: FxHashMap<Head, Vec<u32>>,
    /// Those whose parameter is an interface, which a class or another type may implement.
    ifaces: Vec<u32>,
}

/// The program's function values, so that a call's type tries only those that can accept it, not
/// all of them.
#[derive(Default)]
struct ValueIndex {
    list: Vec<DefId>,
    has: rustc_hash::FxHashSet<DefId>,
    by_arity: FxHashMap<usize, Arity>,
}

impl ValueIndex {
    fn new(cx: &Ctx, values: &[DefId]) -> ValueIndex {
        let mut out = ValueIndex::default();
        for &g in values {
            if !out.has.insert(g) {
                continue;
            }
            let Some(Def::Fn(f)) = &cx.defs[g.0 as usize] else {
                continue;
            };
            let i = out.list.len() as u32;
            out.list.push(g);
            let ps = &f.params[f.captures.len()..];
            let arity = out.by_arity.entry(ps.len()).or_default();
            arity.at.resize_with(ps.len(), Position::default);
            let first = (ps.iter().enumerate())
                .map(|(k, p)| (k, strip(cx, p.ty)))
                .find(|&(_, t)| !is_open(cx, t));
            let Some((k, t)) = first else {
                arity.open.push(i);
                continue;
            };
            let at = &mut arity.at[k];
            at.all.push(i);
            let h = head(cx, t);
            at.by_head.entry(h).or_default().push(i);
            if let Head::Dyn(_) = h {
                at.ifaces.push(i);
            }
        }
        out
    }

    /// Is `g` one of the function values?
    fn has(&self, g: DefId) -> bool {
        self.has.contains(&g)
    }

    /// The function values that may accept arguments of types `want`, in the order the program
    /// makes them; [`unify`] decides.
    fn candidates(&self, cx: &Ctx, want: &[TyId]) -> Vec<DefId> {
        let Some(arity) = self.by_arity.get(&want.len()) else {
            return vec![];
        };
        let mut out: Vec<u32> = arity.open.clone();
        for (at, &w) in arity.at.iter().zip(want) {
            let w = strip(cx, w);
            if is_open(cx, w) {
                out.extend(&at.all);
                continue;
            }
            let mut heads = |h: Head| out.extend(at.by_head.get(&h).into_iter().flatten());
            match head(cx, w) {
                // Its own class, or one it extends.
                Head::Adt(d) => {
                    let mut d = d;
                    for _ in 0..64 {
                        heads(Head::Adt(d));
                        let base = cx.adt(d).and_then(|a| a.base).map(|b| cx.ty.kind(b));
                        let Some(TyKind::Adt(b, _)) = base else { break };
                        d = *b;
                    }
                    out.extend(&at.ifaces);
                }
                Head::Dyn(i) => heads(Head::Dyn(i)),
                h => {
                    heads(h);
                    out.extend(&at.ifaces);
                }
            }
        }
        out.sort_unstable();
        out.into_iter().map(|i| self.list[i as usize]).collect()
    }
}

/// Can a parameter of type `p` be given a value of type `a`? The types unify: the same type,
/// either open (a type parameter, `any`, `unknown`, `object`) at any depth (`T[]` and `string[]`,
/// `Map<K, V>` and `Map<string, App>`), an interface `a` implements, or a base class of `a`.
fn accepts(cx: &Ctx, p: TyId, a: TyId) -> bool {
    unify(cx, p, a, 0)
}

fn unify(cx: &Ctx, p: TyId, a: TyId, depth: u32) -> bool {
    // Deep enough: count it as a match (the join may only grow).
    if depth > 8 {
        return true;
    }
    let (p, a) = (strip(cx, p), strip(cx, a));
    if p == a || is_open(cx, p) || is_open(cx, a) {
        return true;
    }
    let (kp, ka) = (cx.ty.kind(p), cx.ty.kind(a));
    match (kp, ka) {
        (TyKind::Dyn(iface, _), TyKind::Dyn(other, _)) => iface == other,
        (TyKind::Dyn(iface, _), _) => cx.impls.iter().any(|i| i.iface == *iface && i.ty == a),
        (TyKind::Adt(pd, pa), TyKind::Adt(ad, aa)) if pd == ad => {
            pa.len() == aa.len() && pa.iter().zip(aa).all(|(x, y)| unify(cx, *x, *y, depth + 1))
        }
        (TyKind::Adt(pd, _), TyKind::Adt(ad, _)) => descends(cx, *ad, *pd),
        _ if std::mem::discriminant(kp) == std::mem::discriminant(ka) => {
            let (cp, ca) = (crate::types::children(kp), crate::types::children(ka));
            cp.len() == ca.len()
                && cp
                    .iter()
                    .zip(&ca)
                    .all(|(x, y)| unify(cx, *x, *y, depth + 1))
        }
        _ => false,
    }
}
