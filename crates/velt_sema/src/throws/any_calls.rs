//! Errors of functions called through `any` (#1938, #2353; hir_encodings.md "Member reads and
//! calls"). A call through `any` (`AnyApply`, `AnyCall`) runs whatever function the value holds,
//! which may throw, as in JavaScript. Sema can't tell which, so in a program that converts to
//! `any` a value holding a function that throws (a function value, a closure, a method or
//! accessor of a class or object, in the value itself or in its fields, elements and options),
//! every such call in a user module throws `any` ([`FnCx::any_call_op`]), and lowering carries
//! the function's error back from the slot to the caller's `catch` (velt_vir
//! `lower/any_member.rs`, status `ST_THREW`).
//!
//! Whether the program does is a fact of the whole program: what a function throws is known
//! once inference ends. So it is found after inference ([`note`]), like the sets of function
//! types written without `throws` (`fn_types`): `FnTypeSeeds::any_calls_throw`, and the program
//! is checked again with it. A program that converts no throwing function to `any`, or calls
//! nothing through `any` in its own code, is checked and compiled as before.

use rustc_hash::FxHashSet;
use velt_common::std_modules::ANY_CALL_MODULES;
use velt_common::Span;

use crate::body::FnCx;
use crate::ctx::Ctx;
use crate::defs::ThrowSrc;
use crate::hir::{self, TyId, TyKind};

/// How many types the search looks at, at most: a program past it counts as converting a
/// throwing function (a call through `any` throws `any`, which is always sound).
const MAX_TYPES: usize = 1 << 14;

impl FnCx<'_, '_> {
    /// Call `i` through an `any` (`AnyApply` or `AnyCall`): in a user module of a program that
    /// converts a throwing function to `any` (module docs), it throws `any`; so does one in a
    /// standard library module that calls through `any` on its caller's behalf
    /// (`ANY_CALL_MODULES`).
    pub(crate) fn any_call_op(
        &mut self,
        i: hir::Intrinsic,
        args: Vec<hir::Expr>,
        ty: TyId,
        span: Span,
    ) -> hir::Expr {
        let user = !self.cx.scopes[self.module].is_std;
        let path = self.cx.modules[self.module].path.as_str();
        if user || ANY_CALL_MODULES.contains(&path) {
            // Such a module's calls are the program's own (a program that loads `ThisFn`
            // calls through `any` with its helpers).
            self.cx.fn_sites.any_calls = true;
            // In a closure whose function type allows no `any` error, the error leaves only to
            // a `catch` in it; else it is reported as uncaught where it is thrown, as in a
            // program that converts no throwing function (`any_threw_check`): such a closure
            // compiles as it does there.
            let kept = self.f.any_calls_kept && self.f.catch_depth == 0;
            if self.cx.fn_sites.seeds.any_calls_throw && !kept {
                if let Some(any) = self.cx.any_type() {
                    // One source per list: a body's calls through `any` throw the same `any`.
                    let list = self.f.tries.last().unwrap_or(&self.f.uncaught);
                    let seen = |s: &ThrowSrc| matches!(s, ThrowSrc::Direct(t, _) if *t == any);
                    if !list.iter().any(seen) {
                        self.throw_src(ThrowSrc::Direct(any, span));
                        self.cx.fn_sites.any_call_srcs.insert(span);
                    }
                }
            }
        }
        self.intrinsic(i, args, ty, span)
    }

    /// Does the error type `t` a closure is checked against leave out `any` (a function type
    /// written without `throws`, whose inferred errors can't include it, or one whose `throws`
    /// names other types)?
    pub(crate) fn allows_no_any(&mut self, t: TyId) -> bool {
        let Some(any) = self.cx.any_type() else {
            return false;
        };
        // An error set inferred from what is stored into the type takes the `any` too.
        if self.cx.mentions_err_var(t) {
            return false;
        }
        self.cx.error_outside(Some(t), Some(any)).is_some()
    }

    /// Is `t`, the function type a closure is checked against, a site of `throws::fn_types` (a
    /// function type written without `throws` in a user module, a `ProxyHandler` trap)? Its
    /// error type grows with what the closures stored into it throw, `any` too: in a check
    /// after the first it reads as the set found so far, which bounds nothing.
    pub(crate) fn grows_with_closures(&self, t: TyId) -> bool {
        let sites = &self.cx.fn_sites.sites;
        sites.iter().any(|(_, g)| *g == t) || self.cx.var_site(t).is_some()
    }
}

impl Ctx<'_> {
    /// The comparator at `span` of a typed array's `sort` or `toSorted` call that sorts as
    /// before (`FnSites::sort_cmps`) throws `m`, which the comparator type of that sort does not
    /// allow: when `m` is `any` (it calls what calls through `any`), the program is checked
    /// again with the call sorting as an `any[]`'s comparators that may throw do, which sends
    /// the error to the caller (`body/expr/in_place_chain.rs`), and this check's report is
    /// dropped. True when so.
    pub(crate) fn any_sort_found(&mut self, span: Span, m: TyId) -> bool {
        let any = self.any_type();
        let sites = &mut self.fn_sites;
        if any != Some(m) || sites.is_last() || !sites.sort_cmps.contains(&span) {
            return false;
        }
        sites.found.any_sorts.insert(span);
        sites.grew = true;
        true
    }

    /// The error type `t` inferred for function `d`, or, when `d` is a method glue calls, which
    /// cannot throw (`hooks::cannot_throw`), in a program whose calls through `any` throw: what
    /// it throws besides what calls through `any` throw ([`typed_errors`]). Such an error
    /// throws nothing out of it, and lowering reports it as uncaught where it leaves the method
    /// (velt_vir `route_error`), as in a program that converts no throwing function. Its other
    /// errors stay the compile errors they are there.
    pub(crate) fn kept_errors(&mut self, d: hir::DefId, t: Option<TyId>) -> Option<TyId> {
        let any = self.any_type();
        if t.is_none() || any.is_none() || !self.fn_sites.seeds.any_calls_throw {
            return t;
        }
        if !crate::hooks::cannot_throw(self, d) {
            return t;
        }
        let mut open = FxHashSet::default();
        typed_errors(self, d, &mut open)
    }
}

/// What function `d`'s body throws besides what its calls through `any` throw
/// (`FnSites::any_call_srcs`), following the calls of functions whose error type is inferred:
/// a callee's `any` covers whatever else it throws, so its own sources are read instead. Any
/// other source counts with its whole error type, so an `any` that can't be traced to calls
/// through `any` (a call through a function value or an interface, a `throw` of an `any`)
/// stays an error. `open`: the functions being read (a recursive call adds nothing more).
fn typed_errors(cx: &mut Ctx, d: hir::DefId, open: &mut FxHashSet<hir::DefId>) -> Option<TyId> {
    if !open.insert(d) {
        return None;
    }
    let srcs = cx.fn_info(d).throw_srcs.clone();
    let mut all: Vec<TyId> = vec![];
    for s in &srcs {
        let t = match s {
            ThrowSrc::Direct(_, span) if cx.fn_sites.any_call_srcs.contains(span) => None,
            ThrowSrc::Call(g, targs, _) if traced(cx, *g) => {
                let t = typed_errors(cx, *g, open);
                t.map(|t| subst(cx, t, targs))
            }
            _ => super::infer::src_final(cx, s),
        };
        if let Some(t) = t {
            all.extend(cx.error_members(t));
        }
    }
    open.remove(&d);
    cx.error_union(&all)
}

/// Is `g` a function whose error type is inferred from its body's sources ([`typed_errors`]
/// reads them): no written clause, a body checked here, not a member of a dispatch group (an
/// override's errors are its group's)?
fn traced(cx: &mut Ctx, g: hir::DefId) -> bool {
    let f = cx.fn_info(g);
    if f.declared_throws.is_some() || f.source.is_none() {
        return false;
    }
    cx.throw_groups().group_of(g).is_none()
}

/// After inference: when user code calls through `any` and the program converts a throwing
/// function to `any`, and this check didn't assume it, the program is checked again with it
/// (module docs).
pub(super) fn note(cx: &mut Ctx) {
    let sites = &cx.fn_sites;
    if !sites.any_calls || sites.seeds.any_calls_throw || sites.found.any_calls_throw {
        return;
    }
    let held = crate::any_iface_sites::held_types(cx);
    if proxy_call_throws(cx) || holds_throwing(cx, &held) {
        cx.fn_sites.found.any_calls_throw = true;
        cx.fn_sites.grew = true;
    }
}

/// Does a method call on a proxy throw: the `get` trap that reads the method (`[PROXY_GET]`),
/// or an array method run on the proxy, through its traps (`[PROXY_ARRAY_CALL]`)? The proxy's
/// call slot then carries the error back (velt_vir `lower/any_proxy.rs`).
fn proxy_call_throws(cx: &mut Ctx) -> bool {
    let Some(a) = cx.prelude_adt("Proxy").and_then(|p| cx.adt(p)) else {
        return false;
    };
    let names = [
        velt_common::std_key!(proxy_get),
        velt_common::std_key!(proxy_array_call),
    ];
    let defs: Vec<_> = (names.iter())
        .filter_map(|n| a.methods.get(*n).filter(|m| !m.is_static).map(|m| m.def))
        .collect();
    defs.into_iter().any(|m| throws_some(cx, cx.fn_throws(m)))
}

/// Does a value of one of the types `held` hold a function that throws: itself, its fields,
/// elements, options and results, a class's methods and accessors (its subclasses' overrides
/// share their error type: `groups`), the implementations of an interface?
fn holds_throwing(cx: &mut Ctx, held: &[TyId]) -> bool {
    let mut seen: FxHashSet<TyId> = FxHashSet::default();
    let mut todo: Vec<TyId> = held.to_vec();
    while let Some(t) = todo.pop() {
        if !seen.insert(t) {
            continue;
        }
        if seen.len() > MAX_TYPES {
            return true;
        }
        let k = cx.ty.kind(t).clone();
        match &k {
            // The arguments are converted from `any` to the parameter types: not held.
            TyKind::FnPtr { ret, throws, .. } => {
                if throws_some(cx, Some(*throws)) {
                    return true;
                }
                todo.push(*ret);
                continue;
            }
            TyKind::Closure(d) => {
                if throws_some(cx, cx.fn_throws(*d)) {
                    return true;
                }
                todo.push(cx.fn_info(*d).ret);
            }
            // A standard library class holds the program's functions only through its type
            // arguments (a `Map`'s values, a `ThisFn`'s function): its private fields and its
            // methods are its own and don't set the flag (when another value sets it, their
            // errors are carried back too). An object type is the shape of the program's values
            // wherever it was first written.
            TyKind::Adt(d, args) if std_class(cx, *d) => {
                todo.extend(args.iter().copied());
                continue;
            }
            TyKind::Adt(d, args) => {
                let Some(a) = cx.adt(*d) else {
                    // A union holds what its members hold.
                    todo.extend(cx.union_members(t).unwrap_or_default());
                    todo.extend(crate::types::children(&k));
                    continue;
                };
                let fields: Vec<TyId> = a.fields.iter().map(|f| f.ty).collect();
                let methods: Vec<_> = (a.methods.values())
                    .filter(|m| !m.is_static)
                    .map(|m| m.def)
                    .collect();
                let base = a.base;
                for m in methods {
                    if throws_some(cx, cx.fn_throws(m)) {
                        return true;
                    }
                    let r = cx.fn_info(m).ret;
                    todo.push(subst(cx, r, args));
                }
                for f in fields {
                    todo.push(subst(cx, f, args));
                }
                todo.extend(base);
            }
            TyKind::Dyn(i, _) if cx.dynamic(t).is_none() => {
                let impls: Vec<TyId> = (cx.impls.iter())
                    .filter(|m| m.iface == *i)
                    .map(|m| m.ty)
                    .collect();
                todo.extend(impls);
            }
            _ => {}
        }
        todo.extend(crate::types::children(&k));
    }
    false
}

/// Is `d` a class or struct of the standard library (not an object type, `AdtKind::Anon`, nor a
/// union)?
fn std_class(cx: &Ctx, d: hir::DefId) -> bool {
    let class = cx
        .adt(d)
        .is_some_and(|a| !matches!(a.kind, hir::AdtKind::Anon));
    class && crate::any_iface_sites::std_def(cx, d)
}

/// `t` with the type arguments `args` of its definition, when it has any.
fn subst(cx: &mut Ctx, t: TyId, args: &[TyId]) -> TyId {
    match args.is_empty() {
        true => t,
        false => cx.subst(t, args),
    }
}

/// Is the error type `t` (an error-set variable solved) not empty?
fn throws_some(cx: &mut Ctx, t: Option<TyId>) -> bool {
    let t = t.map(|t| match cx.fn_sites.solved.is_empty() {
        true => t,
        false => cx.solved_type(t),
    });
    cx.canon_error(t).is_some()
}
