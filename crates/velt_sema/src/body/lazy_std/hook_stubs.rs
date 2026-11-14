//! `hookCall` of a hook whose module isn't loaded (#2882). A std module that calls a hook by role
//! (`hookCall("sched_blocking_run", …)`, velt_common `std_hooks.rs`) shouldn't load the hook's
//! module because a body that calls it was checked: a body is checked when a checked call asks
//! for its result, or when its module is checked whole, live or not. So the call is checked
//! against a *stub*, a function typed from the table's signature ([`stub`]), and the live walk
//! binds it ([`bind`]): a live definition calling the stub loads the hook's module, and its calls
//! (and what they throw) name the hook's own definition from then on, before the walk records
//! what it reaches. Code that isn't live keeps the stub; lowering never builds it.
//!
//! Binding checks the hook's definition against the stub (its type parameters, parameter types,
//! result and whether it is async) and reports an ICE where they differ, so a table that drifted
//! from std can't type a call wrongly. `velt_sema`'s test `std_hooks.rs` checks the table
//! against std as a whole (`throws` and passing modes included).
//!
//! The module is demanded when the call is checked, as with laziness off, for the program's own
//! code (checked whole, and nearly all live), for a signature the stub can't express (a union, an
//! object-literal type, or a named type no loaded std module, or more than one, knows by that
//! name), when no loader serves std modules on demand, and for a hook that a round before
//! reached only after the passes that follow the check (`Assumed::hooks`): too late to load a
//! module then, so the check runs again with it.

use rustc_hash::FxHashMap;
use velt_common::hook_sig::{FnSig, Mode, Sig, T};
use velt_common::std_hooks::HOOKS;
use velt_common::Span;

use crate::ctx::{Ctx, Item};
use crate::defs::{BodyState, DeclaredThrows, DefInfo, FnInfo, FnKind, ParamSig, ThrowSrc};
use crate::hir::{Callee, DefId, ExprKind as E, FloatTy, IntTy, Nothing, PassMode, TyId, TyKind};

/// The stubs of one check.
#[derive(Clone, Default)]
pub(crate) struct Stubs {
    /// Per hook (its index in `HOOKS`): its stub.
    by_hook: FxHashMap<usize, DefId>,
    /// Per stub: its hook's index, and the hook's definition once bound.
    hooks: FxHashMap<DefId, (usize, Option<DefId>)>,
    /// Per stub: the first call checked against it, where an ICE of its binding points.
    first_call: FxHashMap<DefId, Span>,
    /// Hooks a round before reached only after the passes: no stub for them, their modules are
    /// demanded when the call is checked (`Assumed::hooks`).
    at_check: Vec<usize>,
}

impl Stubs {
    /// Demand the modules of `hooks` when a call of one is checked: a round before reached it
    /// only after the passes.
    pub(super) fn demand_at_check(&mut self, hooks: &[usize]) {
        self.at_check = hooks.to_vec();
    }

    /// Has the check made any stub?
    pub(super) fn any(&self) -> bool {
        !self.hooks.is_empty()
    }

    /// In live code of `caller`: a call `e` of a stub names the hook's definition when it is
    /// bound, and is noted in `unbound` otherwise.
    pub(super) fn note(
        &self,
        caller: DefId,
        e: &mut crate::hir::Expr,
        unbound: &mut Vec<(DefId, DefId)>,
    ) {
        if let E::Call {
            callee: Callee::Def(d, _),
            ..
        } = &mut e.kind
        {
            match self.hooks.get(d) {
                Some((_, Some(real))) => *d = *real,
                Some((_, None)) => unbound.push((caller, *d)),
                None => {}
            }
        }
    }

    /// `caller`'s throw sources name the hooks' definitions where they named a bound stub.
    pub(super) fn rebind_throws(&self, srcs: &mut [ThrowSrc]) {
        for s in srcs {
            if let ThrowSrc::Call(d, ..) | ThrowSrc::Defaults(d, ..) = s {
                if let Some((_, Some(real))) = self.hooks.get(d) {
                    *d = *real;
                }
            }
        }
    }
}

/// The definition a `hookCall` of `HOOKS[index]` in `module` calls when the hook's module isn't
/// loaded: its stub, made the first time. `None` where the module is to be demanded now: laziness
/// is off or checks everything, the passes after the check have run, no loader serves modules on
/// demand, or the stub can't express the signature. `span` is the call's.
pub(crate) fn stub(cx: &mut Ctx, index: usize, module: usize, span: Span) -> Option<DefId> {
    if cx.lazy.modules.is_empty() || cx.lazy.check_all || cx.lazy.finishing() || cx.loads.is_none()
    {
        return None;
    }
    // The program's own code is checked whole and nearly all live: no stub for it.
    let program = !cx.modules[module].is_std || module == cx.root;
    if program || cx.lazy.stubs.at_check.contains(&index) {
        return None;
    }
    if let Some(&d) = cx.lazy.stubs.by_hook.get(&index) {
        return Some(d);
    }
    let Sig::Fn(sig) = &HOOKS[index].sig else {
        return None;
    };
    let info = stub_info(cx, index, sig, module)?;
    let d = cx.alloc_def(Span::DUMMY, DefInfo::Fn(Box::new(info)));
    cx.lazy.stubs.by_hook.insert(index, d);
    cx.lazy.stubs.hooks.insert(d, (index, None));
    cx.lazy.stubs.first_call.insert(d, span);
    Some(d)
}

/// The stub's signature: the table's, with its passing modes fixed.
fn stub_info<'m>(cx: &mut Ctx<'m>, index: usize, sig: &FnSig, module: usize) -> Option<FnInfo<'m>> {
    let mut params = Vec::with_capacity(sig.params.len());
    for (k, p) in sig.params.iter().enumerate() {
        params.push(ParamSig {
            name: format!("p{k}"),
            span: Span::DUMMY,
            ty: ty_of(cx, &p.ty)?,
            mode: match p.mode {
                Mode::Value => PassMode::Copy,
                Mode::Borrow => PassMode::Borrow,
                Mode::BorrowMut => PassMode::BorrowMut,
                Mode::Owned => PassMode::Owned,
            },
            default: None,
        });
    }
    let mut ret = ty_of(cx, &sig.ret)?;
    let mut throws = match &sig.throws {
        Some(t) => Some(ty_of(cx, t)?),
        None => None,
    };
    if sig.is_async {
        // Declared as written, `Promise<R, E>`: what it throws rejects the promise.
        let e = throws.take().unwrap_or(cx.ty.never);
        ret = cx.ty.promise_rejecting(ret, e);
    }
    let name = format!("hookCall:{}", HOOKS[index].name);
    let mut f =
        crate::collect::fn_placeholder(name, Span::DUMMY, Span::DUMMY, module, FnKind::Free, None);
    for g in 0..sig.generics {
        f.generics.push(&T::Param(g).to_string());
    }
    f.params = params;
    f.ret = ret;
    f.fixed_modes = true;
    f.state = BodyState::Done;
    f.is_async = sig.is_async;
    f.declared_throws = throws.map(|t| DeclaredThrows {
        ty: Some(t),
        span: Span::DUMMY,
        from_body: false,
    });
    f.throws = throws;
    Some(f)
}

/// The type `t` of the table's type language, where the stub can express it.
fn ty_of(cx: &mut Ctx, t: &T) -> Option<TyId> {
    let ty = &mut cx.ty;
    let int = |ty: &mut crate::types::Types, i| ty.intern(TyKind::Int(i));
    Some(match t {
        T::I8 => int(ty, IntTy::I8),
        T::I16 => int(ty, IntTy::I16),
        T::I32 => int(ty, IntTy::I32),
        T::I64 => int(ty, IntTy::I64),
        T::ISize => int(ty, IntTy::ISize),
        T::U8 => int(ty, IntTy::U8),
        T::U16 => int(ty, IntTy::U16),
        T::U32 => int(ty, IntTy::U32),
        T::U64 => int(ty, IntTy::U64),
        T::USize => int(ty, IntTy::USize),
        T::F32 => ty.intern(TyKind::Float(FloatTy::F32)),
        T::F64 => ty.intern(TyKind::Float(FloatTy::F64)),
        T::Bool => ty.intern(TyKind::Bool),
        T::Str => ty.intern(TyKind::Str),
        T::Void => ty.unit,
        T::Never => ty.never,
        T::Symbol => ty.intern(TyKind::Symbol),
        T::Param(n) => ty.param(*n),
        T::Array(e) => {
            let e = ty_of(cx, e)?;
            cx.ty.array(e)
        }
        T::Tuple(ts) => {
            let ts = ts
                .iter()
                .map(|t| ty_of(cx, t))
                .collect::<Option<Vec<_>>>()?;
            cx.ty.intern(TyKind::Tuple(ts))
        }
        T::OrNull(t) => {
            let t = ty_of(cx, t)?;
            cx.ty.option_of(t, Nothing::Null)
        }
        T::OrUndefined(t) => {
            let t = ty_of(cx, t)?;
            cx.ty.option_of(t, Nothing::Undefined)
        }
        T::Promise(v, e) => {
            let (v, e) = (ty_of(cx, v)?, ty_of(cx, e)?);
            cx.ty.promise_rejecting(v, e)
        }
        T::Fn {
            params,
            ret,
            throws,
            rest,
        } => {
            let params = params
                .iter()
                .map(|t| ty_of(cx, t))
                .collect::<Option<Vec<_>>>()?;
            let ret = ty_of(cx, ret)?;
            let throws = match throws {
                Some(t) => ty_of(cx, t)?,
                None => cx.ty.never,
            };
            cx.ty.intern(TyKind::FnPtr {
                params,
                ret,
                throws,
                rest: *rest,
            })
        }
        T::Named(n, args) if !n.starts_with('{') => {
            let args = args
                .iter()
                .map(|t| ty_of(cx, t))
                .collect::<Option<Vec<_>>>()?;
            let d = named(cx, n)?;
            match (cx.adt(d).is_some(), cx.iface(d).is_some()) {
                (true, _) => cx.ty.intern(TyKind::Adt(d, args)),
                (_, true) => cx.ty.intern(TyKind::Dyn(d, args)),
                _ => return None,
            }
        }
        T::Union(_) | T::Named(..) => return None,
    })
}

/// The std class, struct or interface named `name` (the table's last name segment): the one
/// definition the loaded std modules know by that name, own or imported. `None` when none does
/// (its module isn't loaded) or several do.
fn named(cx: &Ctx, name: &str) -> Option<DefId> {
    let mut found = None;
    for (m, scope) in cx.scopes.iter().enumerate() {
        if !cx.modules.get(m).is_some_and(|sm| sm.is_std) {
            continue;
        }
        if let Some(Item::Def(d)) = scope.items.get(name) {
            match found {
                None => found = Some(*d),
                Some(f) if f == *d => {}
                Some(_) => return None,
            }
        }
    }
    found
}

/// Binds the stubs that live code calls (`(caller, stub)`): loads each hook's module, checks its
/// definition against the stub, and makes the callers' calls and throw sources name it. Gives
/// what each bound stub now is.
pub(super) fn bind(cx: &mut Ctx, calls: &[(DefId, DefId)]) -> FxHashMap<DefId, DefId> {
    let mut bound = FxHashMap::default();
    for &(_, s) in calls {
        if bound.contains_key(&s) {
            continue;
        }
        let Some(&(index, done)) = cx.lazy.stubs.hooks.get(&s) else {
            continue;
        };
        let real = match done {
            Some(real) => real,
            None => match bind_one(cx, index, s) {
                Some(real) => real,
                None => continue,
            },
        };
        bound.insert(s, real);
    }
    let mut callers: Vec<DefId> = calls.iter().map(|&(c, _)| c).collect();
    callers.sort_unstable();
    callers.dedup();
    for c in callers {
        rebind(cx, c);
    }
    bound
}

/// Loads hook `index`'s module and finds its definition, checked against stub `s`.
fn bind_one(cx: &mut Ctx, index: usize, s: DefId) -> Option<DefId> {
    let h = &HOOKS[index];
    let at = cx
        .lazy
        .stubs
        .first_call
        .get(&s)
        .copied()
        .unwrap_or(Span::DUMMY);
    if cx.lazy.finishing() {
        // Reached by what the passes after the check made: the next round demands its module
        // when the call is checked.
        cx.lazy.live.late.hooks.push(index);
        return None;
    }
    cx.demand_std(&h.module());
    let real = match cx.std_def(&h.qualified()) {
        Some(Item::Def(d)) if matches!(cx.info[d.0 as usize], DefInfo::Fn(_)) => d,
        _ => {
            let msg = format!(
                "ICE: `hookCall`'s hook `{}` ({}) is not loaded",
                h.name,
                h.qualified()
            );
            cx.err(msg, at);
            return None;
        }
    };
    if let Some(why) = differs(cx, s, real) {
        let msg = format!(
            "ICE: the hook `{}` ({}) differs from its signature in velt_common's `std_hooks.rs`: {why}",
            h.name,
            h.qualified()
        );
        cx.err(msg, at);
        return None;
    }
    if let Some(e) = cx.lazy.stubs.hooks.get_mut(&s) {
        e.1 = Some(real);
    }
    Some(real)
}

/// How hook definition `real` differs from stub `s` (`None`: it doesn't).
fn differs(cx: &Ctx, s: DefId, real: DefId) -> Option<String> {
    let (s, r) = (cx.fn_info(s), cx.fn_info(real));
    if s.generics.len() != r.generics.len() {
        return Some(format!("{} type parameters", r.generics.len()));
    }
    if s.params.len() != r.params.len() {
        return Some(format!("{} parameters", r.params.len()));
    }
    if let Some(k) = (s.params.iter().zip(&r.params)).position(|(a, b)| a.ty != b.ty) {
        return Some(format!("parameter {} has another type", k + 1));
    }
    if s.ret != r.ret {
        return Some("the result has another type".to_string());
    }
    if s.is_async != r.is_async {
        return Some(format!("async: {}", r.is_async));
    }
    None
}

/// The calls and throw sources of `caller`'s checked code name the hooks' definitions where they
/// named a bound stub.
fn rebind(cx: &mut Ctx, caller: DefId) {
    let i = caller.0 as usize;
    let stubs = &cx.lazy.stubs;
    let mut unbound = vec![];
    if let Some(crate::hir::Def::Fn(f)) = &mut cx.defs[i] {
        crate::visit::exprs_mut(&mut f.body.block, &mut |e| {
            stubs.note(caller, e, &mut unbound)
        });
    }
    match &mut cx.info[i] {
        DefInfo::Fn(f) => {
            stubs.rebind_throws(&mut f.throw_srcs);
            for e in f.params.iter_mut().filter_map(|p| p.default.as_mut()) {
                crate::visit::exprs_in(e, &mut |e| stubs.note(caller, e, &mut unbound));
            }
        }
        // A module constant's or slot's initializer.
        DefInfo::Global(g) => {
            if let Some(e) = &mut g.init {
                crate::visit::exprs_in(e, &mut |e| stubs.note(caller, e, &mut unbound));
            }
        }
        _ => {}
    }
}
