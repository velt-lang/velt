//! The instantiation sites of a body besides a direct generic call (`Callee::Def`), for the passes
//! that carry a generic definition's requirements to its concrete instances
//! (`record_keys::check_instantiations`, `type_level::instances`):
//!
//! - a function used as a value (`FnRef`, also a static method), which lowering instantiates
//!   with the reference's type arguments like a call;
//! - `new C<A>(…)` and a literal of `C<A>`: the constructor runs with the arguments of the class
//!   that declares it, and the field initializers of `C` and of each base class with theirs
//!   (the initializers of a class are a unit of their own, keyed by the class: [`for_each_init`]);
//! - a method call through a type parameter with type arguments of its own
//!   (`s.area<string>(…)`, `s: S`, `S extends Sh`): which method runs is only known once `S`
//!   is ([`ParamCall`]).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{class_chain, Rounds};
use crate::hir::{AdtKind, Callee, Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::types::collect_params;
use crate::types::Types;
use crate::visit;

/// The types one function body (or one type's field initializers) mentions, and the
/// definitions it instantiates.
#[derive(Default)]
pub(crate) struct Uses {
    /// Every type mentioned (expressions, locals, type arguments), with its first span.
    pub types: Vec<(TyId, Span)>,
    pub calls: Vec<(DefId, Vec<TyId>, Span)>,
    /// The guard bits of each of `calls` ([`carry_guard`]).
    pub guards: Vec<u64>,
    pub closures: Vec<DefId>,
    /// The methods the class types among `types` dispatch dynamically to, with those types'
    /// arguments and the span of the type (filled by the pass: `dispatch::Dispatch`).
    pub dyn_calls: Vec<(DefId, Vec<TyId>, Span)>,
    pub param_calls: Vec<(ParamCall, Span)>,
}

impl Uses {
    /// The defs whose requirements this unit's requirements are made of (those of param calls
    /// are found during the visits: [`resolve_param_calls`]).
    pub(crate) fn reads(&self) -> impl Iterator<Item = DefId> + '_ {
        let calls = self.calls.iter().map(|(d, _, _)| *d);
        let dyn_calls = self.dyn_calls.iter().map(|(d, _, _)| *d);
        calls.chain(self.closures.iter().copied()).chain(dyn_calls)
    }

    /// The work of one visit, for [`crate::dispatch::instantiation_work`].
    pub(crate) fn size(&self) -> u64 {
        let n = self.types.len() + self.calls.len() + self.closures.len();
        (n + self.dyn_calls.len() + self.param_calls.len()) as u64
    }
}

/// The [`Uses`] of every function body (with its parameter and result types if `signature`),
/// in definition order, followed by those of the field initializers of each struct and class (a
/// unit of their own, keyed by the type, which `new` and literals of the type instantiate).
pub(crate) fn collect(cx: &mut Ctx, signature: bool) -> Vec<(DefId, Uses)> {
    collect_with(cx, signature, 0, true, false)
}

/// [`collect`] of the definitions from `from` on (those made since an earlier collection);
/// without `types`, [`Uses::types`] stays empty (the calls are all a caller needs, when no
/// method can be dispatched to). The initializers of module variables are units too, keyed by
/// the variable (a module constant made by `JSON.parse` instantiates its generated decoder).
pub(crate) fn collect_from(
    cx: &mut Ctx,
    signature: bool,
    from: usize,
    types: bool,
) -> Vec<(DefId, Uses)> {
    collect_with(cx, signature, from, types, true)
}

fn collect_with(
    cx: &mut Ctx,
    signature: bool,
    from: usize,
    types: bool,
    globals: bool,
) -> Vec<(DefId, Uses)> {
    type News = Vec<(DefId, Vec<TyId>, Span, u64)>;
    let mut out: Vec<(DefId, Uses, News)> = vec![];
    let mut converts = cx.type_level.converts.clone();
    converts.extend(cx.type_level.inline.reads.iter().map(|(&k, &v)| (k, v)));
    let tys = &cx.ty;
    for (i, d) in cx.defs.iter_mut().enumerate().skip(from) {
        let Some(Def::Fn(f)) = d else { continue };
        let mut u = Uses::default();
        let mut news = vec![];
        let mut seen: FxHashSet<TyId> = FxHashSet::default();
        let guards = guards_of(tys, &mut f.body.block);
        if signature && types {
            let span = f.body.block.span;
            for t in f.params.iter().map(|p| p.ty).chain([f.ret]) {
                if seen.insert(t) {
                    u.types.push((t, span));
                }
            }
        }
        for l in f.body.locals.iter().filter(|_| types) {
            if seen.insert(l.ty) {
                u.types.push((l.ty, l.span));
            }
        }
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            let guard = guards.get(&(e as *const Expr)).copied().unwrap_or(0);
            add_site(tys, &mut u, &mut news, &mut seen, e, guard, types);
            if !converts.is_empty() {
                convert_site(&mut u, &mut seen, e, &converts);
            }
        });
        out.push((DefId(i as u32), u, news));
    }
    let mut inits: FxHashMap<DefId, (Uses, News, FxHashSet<TyId>)> = FxHashMap::default();
    for_each_init(cx, globals, &mut |tys, d, e| {
        if (d.0 as usize) < from {
            return;
        }
        let (u, news, seen) = inits.entry(d).or_default();
        add_site(tys, u, news, seen, e, 0, types);
        if !converts.is_empty() {
            convert_site(u, seen, e, &converts);
        }
    });
    // A module constant's initializer: lowering evaluates it where it is read
    // (`hir::GlobalDef::init`), in whichever function reads it, so it is a unit of its own.
    for (i, info) in cx.info.iter_mut().enumerate().skip(from) {
        let DefInfo::Global(g) = info else { continue };
        let Some(init) = g.init.as_mut() else {
            continue;
        };
        let (u, news, seen) = inits.entry(DefId(i as u32)).or_default();
        visit::exprs_in(init, &mut |e: &mut Expr| {
            add_site(&cx.ty, u, news, seen, e, 0, types);
            if !converts.is_empty() {
                convert_site(u, seen, e, &converts);
            }
        });
    }
    let mut inits: Vec<_> = inits.into_iter().collect();
    inits.sort_by_key(|(d, _)| *d);
    out.extend(inits.into_iter().map(|(d, (u, news, _))| (d, u, news)));
    out.into_iter()
        .map(|(d, mut u, news)| {
            for (c, targs, span, guard) in news {
                for (callee, args) in constructed(cx, c, targs) {
                    u.calls.push((callee, args, span));
                    u.guards.push(guard);
                }
            }
            (d, u)
        })
        .collect()
}

/// Records the requirement (`TyOp::Convert`) of conversion `e` (an `Upcast` between two types
/// whose fields are known only per instantiation, `crate::generic_spread`) as a type `e`
/// mentions, so each instantiation reduces it.
fn convert_site(
    u: &mut Uses,
    seen: &mut FxHashSet<TyId>,
    e: &Expr,
    converts: &FxHashMap<(TyId, TyId, Span), TyId>,
) {
    use crate::hir::{ExprKind as H, Intrinsic};
    let key = match &e.kind {
        H::Upcast(inner) => Some((inner.ty, e.ty, e.span)),
        H::Call {
            callee: crate::hir::Callee::Intrinsic(Intrinsic::KeyRead | Intrinsic::KeyWrite),
            args,
        } => Some((args[0].ty, args[1].ty, e.span)),
        H::Call {
            callee: crate::hir::Callee::Intrinsic(Intrinsic::SpreadObject),
            ..
        } => Some((e.ty, e.ty, e.span)),
        _ => None,
    };
    if let Some(key) = key {
        if let Some(&op) = converts.get(&key) {
            if seen.insert(op) {
                u.types.push((op, e.span));
            }
        }
    }
}

/// Records what expression `e` mentions and instantiates in `u` (`news`: the types it
/// constructs, resolved by [`constructed`] afterwards; `guard`: its guard bits).
fn add_site(
    tys: &Types,
    u: &mut Uses,
    news: &mut Vec<(DefId, Vec<TyId>, Span, u64)>,
    seen: &mut FxHashSet<TyId>,
    e: &Expr,
    guard: u64,
    types: bool,
) {
    if types && seen.insert(e.ty) {
        u.types.push((e.ty, e.span));
    }
    let mut mention = |u: &mut Uses, ts: &[TyId]| {
        for t in ts.iter().filter(|_| types) {
            if seen.insert(*t) {
                u.types.push((*t, e.span));
            }
        }
    };
    match site(tys, e) {
        Some(Site::Call(d, targs)) => {
            mention(u, &targs);
            u.calls.push((d, targs, e.span));
            u.guards.push(guard);
        }
        Some(Site::Closure(c)) => u.closures.push(c),
        Some(Site::New(d, targs)) => news.push((d, targs, e.span, guard)),
        Some(Site::Param(p)) => {
            mention(u, &p.margs);
            u.param_calls.push((p, e.span));
        }
        None => {}
    }
}

/// One instantiation site.
enum Site {
    /// A direct call or a function value: the definition and its type arguments.
    Call(DefId, Vec<TyId>),
    Closure(DefId),
    /// `new C<A>(…)` or a literal of `C<A>`: see [`constructed`].
    New(DefId, Vec<TyId>),
    Param(ParamCall),
}

/// The site expression `e` is, if any.
fn site(tys: &Types, e: &Expr) -> Option<Site> {
    Some(match &e.kind {
        E::Call {
            callee: Callee::Def(d, targs),
            ..
        }
        | E::FnRef(d, targs)
            if !targs.is_empty() =>
        {
            Site::Call(*d, targs.clone())
        }
        E::Closure(c) => Site::Closure(*c),
        E::New { def, type_args, .. } | E::AdtLit { def, type_args, .. } => {
            Site::New(*def, type_args.clone())
        }
        E::Call {
            callee:
                Callee::ParamMethod {
                    iface,
                    slot,
                    method_type_args,
                    ..
                },
            args,
        } if !method_type_args.is_empty() => Site::Param(ParamCall {
            recv: args.first()?.ty,
            iface: *iface,
            slot: *slot,
            margs: method_type_args.clone(),
        }),
        // A generic method called on an interface value runs the method of each
        // implementation ([`ParamCall::targets`]).
        E::Call {
            callee:
                Callee::Dyn {
                    slot,
                    method_type_args,
                },
            args,
        } if !method_type_args.is_empty() => {
            let recv = args.first()?.ty;
            let TyKind::Dyn(iface, _) = tys.kind(recv) else {
                return None;
            };
            Site::Param(ParamCall {
                recv,
                iface: *iface,
                slot: *slot,
                margs: method_type_args.clone(),
            })
        }
        _ => return None,
    })
}

/// What constructing `def<targs>` instantiates: its constructor (with the arguments of the
/// class that declares it), and the initializer unit of it and each base class.
fn constructed(cx: &mut Ctx, def: DefId, targs: Vec<TyId>) -> Vec<(DefId, Vec<TyId>)> {
    let Some(ctor) = cx
        .adt(def)
        .filter(|a| a.kind != AdtKind::Anon)
        .map(|a| a.ctor)
    else {
        return vec![];
    };
    let chain = class_chain(cx, def, targs);
    let owner = ctor.and_then(|c| match &cx.info[c.0 as usize] {
        DefInfo::Fn(f) => f.owner,
        _ => None,
    });
    let mut out = vec![];
    for (c, args) in chain {
        if args.is_empty() {
            continue;
        }
        if let Some(ctor) = ctor.filter(|_| Some(c) == owner) {
            out.push((ctor, args.clone()));
        }
        out.push((c, args));
    }
    out
}

/// Visits the field initializers of every struct and class (own fields and the inherited ones
/// it gives an initializer), in terms of its type parameters, with the type's def.
fn for_each_init(cx: &mut Ctx, globals: bool, f: &mut dyn FnMut(&Types, DefId, &mut Expr)) {
    let tys = &cx.ty;
    for (i, info) in cx.info.iter_mut().enumerate() {
        let d = DefId(i as u32);
        if let DefInfo::Global(g) = info {
            if let Some(e) = g.init.as_mut().filter(|_| globals) {
                crate::visit::exprs_in(e, &mut |e| f(tys, d, e));
            }
            continue;
        }
        let DefInfo::Adt(a) = info else { continue };
        let start = a.own_fields_start;
        let own = a.fields[start..]
            .iter_mut()
            .filter_map(|x| x.default.as_mut());
        let redefined = a.redefined.iter_mut().filter_map(|r| r.init.as_mut());
        for e in own.chain(redefined) {
            crate::visit::exprs_in(e, &mut |e| f(tys, d, e));
        }
    }
}

/// A call of a generic interface method through a type parameter with type arguments of its
/// own (`s.area<string>(…)`): the implementing method of the receiver's type runs, with that
/// type's arguments followed by `margs` (as lowering resolves it).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ParamCall {
    pub recv: TyId,
    pub iface: DefId,
    pub slot: u32,
    pub margs: Vec<TyId>,
}

impl ParamCall {
    pub(crate) fn subst(&self, cx: &mut Ctx, args: &[TyId]) -> ParamCall {
        ParamCall {
            recv: cx.subst(self.recv, args),
            iface: self.iface,
            slot: self.slot,
            margs: self.margs.iter().map(|t| cx.subst(*t, args)).collect(),
        }
    }

    /// Instantiated with the arguments a dynamically dispatched method runs with; `None` if it
    /// mentions the method's own type parameters (as `dispatch::instantiate`).
    pub(crate) fn instantiate(&self, cx: &mut Ctx, args: &[TyId]) -> Option<ParamCall> {
        let mut ps = vec![];
        for t in std::iter::once(&self.recv).chain(&self.margs) {
            collect_params(&cx.ty, *t, &mut ps);
        }
        ps.iter()
            .all(|p| (*p as usize) < args.len())
            .then(|| self.subst(cx, args))
    }

    /// Is the receiver's type still a type parameter (or made of one)?
    fn generic(&self, cx: &Ctx) -> bool {
        let mut ps = vec![];
        collect_params(&cx.ty, self.recv, &mut ps);
        !ps.is_empty()
    }

    /// The method that runs for a concrete receiver, with its type arguments.
    pub(crate) fn resolve(&self, cx: &mut Ctx) -> Option<(DefId, Vec<TyId>)> {
        let (index, _, implementor) = cx.find_impl(self.recv, self.iface)?;
        self.method_of(cx, index, implementor)
    }

    /// The methods that may run for a concrete receiver: the one [`Self::resolve`] finds, or for
    /// a generic method called on an interface value (`Callee::Dyn`), the method of every
    /// implementation of the interface, for each type the program has made that it implements
    /// it for (an implementation that is not generic has its one type).
    pub(crate) fn targets(&self, cx: &mut Ctx) -> Vec<(DefId, Vec<TyId>)> {
        let TyKind::Dyn(i, dargs) = cx.ty.kind(self.recv).clone() else {
            return self.resolve(cx).into_iter().collect();
        };
        if i != self.iface {
            return vec![];
        }
        let mut out = vec![];
        for index in 0..cx.impls.len() as u32 {
            let imp = &cx.impls[index as usize];
            if imp.iface != i {
                continue;
            }
            // A generic closure made in a generic function runs at that function's type
            // arguments too, which are not known here.
            if let TyKind::Closure(c) = *cx.ty.kind(imp.ty) {
                let own = match &cx.info[c.0 as usize] {
                    DefInfo::Fn(f) => f.generics.len(),
                    _ => 0,
                };
                if own != self.margs.len() {
                    continue;
                }
            }
            let found = match imp.generics {
                0 => cx.try_impl(index, imp.ty).into_iter().collect(),
                _ => made_instances(cx, index),
            };
            for (_, iargs, implementor) in found {
                if iargs == dargs {
                    out.extend(self.method_of(cx, index, implementor));
                }
            }
            // A generic implementation may also be made only where a generic function is
            // instantiated (`new TG<X[]>` in `mk<X>`), which interns no concrete type here:
            // the method at the call's type arguments, its owner's left unknown (`error`), so
            // what it needs of those alone is still reached.
            let imp = &cx.impls[index as usize];
            let (generics, pattern, iargs) = (imp.generics, imp.ty, imp.iface_args.clone());
            if generics > 0 && iargs == dargs {
                let unknown = vec![cx.ty.error; generics as usize];
                let implementor = cx.subst(pattern, &unknown);
                out.extend(self.method_of(cx, index, implementor));
            }
        }
        out
    }

    /// The method of impl `index` for `implementor` that this call runs, with its type
    /// arguments.
    fn method_of(&self, cx: &mut Ctx, index: u32, implementor: TyId) -> Option<(DefId, Vec<TyId>)> {
        let imp = cx.impls.get(index as usize)?;
        let (pattern, generics, iface_args) = (imp.ty, imp.generics, imp.iface_args.clone());
        let method = *imp.methods.get(self.slot as usize)?;
        let is_default = cx
            .iface(self.iface)
            .and_then(|i| i.methods.get(self.slot as usize))
            .is_some_and(|m| m.default == Some(method));
        let mut binds = vec![None; generics as usize];
        cx.match_ty(pattern, implementor, &mut binds);
        let binds: Vec<TyId> = binds
            .into_iter()
            .map(|b| b.unwrap_or(cx.ty.error))
            .collect();
        let mut args: Vec<TyId> = if is_default {
            let mut a: Vec<TyId> = iface_args.iter().map(|t| cx.subst(*t, &binds)).collect();
            a.push(implementor);
            a
        } else {
            binds
        };
        args.extend(self.margs.iter().copied());
        Some((method, args))
    }
}

/// The concrete types generic impl `index` implements its interface for, among the types
/// interned so far, as `Ctx::try_impl` gives them.
fn made_instances(cx: &mut Ctx, index: u32) -> Vec<(u32, Vec<TyId>, TyId)> {
    let pattern = cx.impls[index as usize].ty;
    let shape = std::mem::discriminant(cx.ty.kind(pattern));
    let mut out = vec![];
    for t in 0..cx.ty.table.len() as u32 {
        let t = TyId(t);
        if std::mem::discriminant(cx.ty.kind(t)) != shape {
            continue;
        }
        let mut ps = vec![];
        collect_params(&cx.ty, t, &mut ps);
        if ps.is_empty() {
            out.extend(cx.try_impl(index, t));
        }
    }
    out
}

/// What a pass takes from each method a param call resolves to (see [`resolve_param_calls`]).
pub(crate) type MethodReqs<'a> = dyn FnMut(&mut Ctx, DefId, &[TyId]) -> Vec<ParamCall> + 'a;

/// Upper bound on the param calls one visit resolves (a method whose param calls grow their
/// own receiver would otherwise never end).
const RESOLVE_LIMIT: usize = 10_000;

/// Resolves the param calls that reach the function at `at` (`Rounds` index): those whose
/// receiver is concrete run a known method, which the function now reads ([`Rounds::read`]);
/// `method` takes each (method, type arguments) and returns the method's own param calls,
/// instantiated. Returns the ones still generic: requirements of the function itself.
pub(crate) fn resolve_param_calls(
    cx: &mut Ctx,
    rounds: &mut Rounds,
    at: usize,
    pending: Vec<ParamCall>,
    method: &mut MethodReqs,
) -> Vec<ParamCall> {
    let mut generic = vec![];
    let mut done: FxHashSet<ParamCall> = FxHashSet::default();
    let mut stack = pending;
    while let Some(p) = stack.pop() {
        if done.len() >= RESOLVE_LIMIT || !done.insert(p.clone()) {
            continue;
        }
        if p.generic(cx) {
            generic.push(p);
            continue;
        }
        for (m, args) in p.targets(cx) {
            rounds.read(at, m);
            stack.extend(method(cx, m, &args));
        }
        // The call may run the method of an instance of a generic implementation found later
        // (`type_level::instances`, which reports it as a change of the class).
        if let TyKind::Dyn(i, _) = cx.ty.kind(p.recv).clone() {
            for imp in cx.impls.iter().filter(|m| m.iface == i && m.generics > 0) {
                if let TyKind::Adt(d, _) = cx.ty.kind(imp.ty) {
                    rounds.read(at, *d);
                }
            }
        }
    }
    generic
}

/// Guard bits (a bit per type parameter whose primitive guard encloses a call or a
/// requirement: `visit::primitive_guards`, issue #1251) of a requirement of `g` with bits
/// `need`, carried to a call `g<targs>` with bits `site` in the caller: `None` when a guarded
/// parameter of `g` is given an object type, so the branch the requirement comes from is not
/// instantiated there; else `site` and the caller's parameter each guarded one is given as is
/// (any other argument may hold a primitive in every instance: that guard is dropped).
pub(crate) fn carry_guard(cx: &Ctx, need: u64, targs: &[TyId], site: u64) -> Option<u64> {
    let mut out = site;
    for (q, t) in targs.iter().enumerate().take(64) {
        if need & (1 << q) == 0 {
            continue;
        }
        if cx.object_only(*t) {
            return None;
        }
        if let crate::hir::TyKind::Param(p) = cx.ty.kind(*t) {
            out |= 1u64.checked_shl(*p).unwrap_or(0);
        }
    }
    Some(out)
}

/// The guard bits of each expression of `b` (`visit::primitive_guards`), keyed by address.
pub(crate) fn guards_of(
    ty: &crate::types::Types,
    b: &mut crate::hir::Block,
) -> FxHashMap<*const Expr, u64> {
    let param_of = |t: TyId| match ty.kind(t) {
        crate::hir::TyKind::Param(p) => Some(*p),
        _ => None,
    };
    visit::primitive_guards(b, &param_of)
}
