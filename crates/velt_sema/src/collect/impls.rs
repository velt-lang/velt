//! Phase 5: `implements` checking. For every `class/struct C implements I<args>` the class must
//! have I's fields (same name and type) and a method for every interface method without a
//! default (same parameter and return types). Each pair becomes a `hir::ImplDef` whose
//! `methods` list the implementation per interface slot (own, inherited, or I's default),
//! followed by one synthesized getter per interface field ([`getters`](super::getters)) and,
//! once the program writes through the interface, a setter per writable field
//! (`crate::iface_writes`).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Diagnostic;

use super::forwarders::{synth_method, Host, Target};
use super::lookup::{lookup_method, Found};
use super::shapes::self_type;
use crate::ctx::Ctx;
use crate::defs::{Bound, IfaceMethod};
use crate::hir::{DefId, ImplDef, TyId};
use crate::iface_writes::SetterSrc;

pub(super) fn build_impls(cx: &mut Ctx) {
    let adts: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| cx.adt(*d).is_some_and(|a| !a.implements.is_empty()))
        .collect();
    let mut getters = Getters::default();
    for d in adts {
        let implements = cx.adt(d).expect("ICE: adt").implements.clone();
        for b in implements {
            let span = cx.adt(d).map_or(velt_common::Span::DUMMY, |a| a.span);
            if cx.refuse_known_iface(b.iface, span) {
                continue;
            }
            if let Some((imp, setters)) = check_impl(cx, d, &b, &mut getters) {
                cx.impls.push(imp);
                // A field with a getter and no setter leaves the implementation without setters.
                if let Some(setters) = setters {
                    cx.note_setters(cx.impls.len() as u32 - 1, setters);
                }
            }
        }
    }
    override_impls(cx, &mut getters);
}

/// A class overriding a generic method of an interface that a base class implements gets an
/// implementation of its own: a call through a type-parameter bound (`s.area<U>(…)` with
/// `S extends Sh`) runs the method of the receiver's nearest implementation, statically, so
/// that must be the override (issue #1132). Generic methods have no vtable slot to dispatch
/// through instead.
fn override_impls(cx: &mut Ctx, getters: &mut Getters) {
    let built = cx.impls.len() as u32;
    let mut done: FxHashSet<(DefId, DefId)> = FxHashSet::default();
    for (d, key) in cx.generic_overrides.clone() {
        let n = cx.adt(d).map_or(0, |a| a.generics.len());
        let self_ty = self_type(cx, d, n);
        for b in cx.adt(d).map(|a| a.implements.clone()).unwrap_or_default() {
            done.insert((d, b.iface));
        }
        let mut bounds = vec![];
        let mut cur = cx.base_of(self_ty);
        for _ in 0..64 {
            let Some(t) = cur else { break };
            let class = cx.class_of(t).map(|(c, _)| c);
            for i in 0..built {
                let imp = &cx.impls[i as usize];
                let (ty, iface) = (imp.ty, imp.iface);
                let own =
                    matches!(cx.ty.kind(ty), crate::hir::TyKind::Adt(c, _) if Some(*c) == class);
                let declares = cx.iface(iface).is_some_and(|f| {
                    f.methods
                        .iter()
                        .any(|m| m.name == key && m.generics.len() > 0)
                });
                if !own || !declares || !done.insert((d, iface)) {
                    continue;
                }
                if let Some((_, args, _)) = cx.try_impl(i, t) {
                    bounds.push(Bound { iface, args });
                }
            }
            cur = cx.base_of(t);
        }
        for b in bounds {
            if let Some((imp, setters)) = check_impl(cx, d, &b, getters) {
                cx.impls.push(imp);
                if let Some(setters) = setters {
                    cx.note_setters(cx.impls.len() as u32 - 1, setters);
                }
            }
        }
    }
}

/// (type, field index) → its getter, shared by every interface that declares the field (an
/// interface and the ones it extends).
type Getters = FxHashMap<(DefId, usize), DefId>;

/// One `C implements I<args>` being checked.
struct Pair<'b> {
    d: DefId,
    self_ty: TyId,
    bound: &'b Bound,
    cname: String,
    iname: String,
    span: velt_common::Span,
}

fn check_impl(
    cx: &mut Ctx,
    d: DefId,
    b: &Bound,
    cache: &mut Getters,
) -> Option<(ImplDef, Option<Vec<SetterSrc>>)> {
    let a = cx.adt(d).expect("ICE: adt");
    let (n, span, cname) = (a.generics.len(), a.span, a.name.clone());
    let self_ty = self_type(cx, d, n);
    let iface = cx.iface(b.iface)?;
    let (iname, imethods) = (iface.name.clone(), iface.methods.clone());
    let pair = Pair {
        d,
        self_ty,
        bound: b,
        cname,
        iname,
        span,
    };
    let (getters, setters) = field_getters(cx, &pair, cache);
    let mut methods = vec![];
    for m in &imethods {
        methods.push(impl_method(cx, &pair, m)?);
    }
    methods.extend(getters);
    let imp = ImplDef {
        ty: self_ty,
        generics: n as u32,
        iface: b.iface,
        iface_args: b.args.clone(),
        methods,
    };
    Some((imp, setters))
}

/// The class must have every interface field (same type, or for an optional one its type
/// without `undefined`); each gets a getter and a setter
/// source (`crate::iface_writes`), or the implementation gets none (`None`: a writable field
/// with a getter and no setter).
fn field_getters(
    cx: &mut Ctx,
    p: &Pair,
    cache: &mut Getters,
) -> (Vec<DefId>, Option<Vec<SetterSrc>>) {
    let ifields = cx
        .iface(p.bound.iface)
        .map(|i| i.fields.clone())
        .unwrap_or_default();
    let (mut getters, mut setters) = (vec![], Some(vec![]));
    for f in ifields {
        let want = cx.subst(f.ty, &p.bound.args);
        let have = cx
            .adt(p.d)
            .and_then(|a| a.fields.iter().position(|g| g.name == f.name))
            .map(|i| (i, cx.adt(p.d).expect("ICE: adt").fields[i].ty));
        // The same type, or the same but for error-set variables (`throws::vars`).
        if let Some((i, ty)) = have.filter(|(_, ty)| cx.same_ty(*ty, want)) {
            let g = match cache.get(&(p.d, i)) {
                Some(&g) => g,
                None => super::getters::field_getter(cx, p.d, p.self_ty, i, ty),
            };
            cache.insert((p.d, i), g);
            getters.push(g);
            if let Some(setters) = setters.as_mut() {
                setters.push(SetterSrc::Field {
                    d: p.d,
                    self_ty: p.self_ty,
                    index: i,
                    ty,
                });
            }
        } else if let Some((i, ty)) = have.filter(|(_, ty)| {
            f.optional && cx.ty.opt_payload(want).is_some_and(|w| cx.same_ty(*ty, w))
        }) {
            // A required field provides an optional one (`x: number` for `x?: number`), as in
            // TypeScript and as for an object type (`object_ifaces`): read, it is present;
            // writing `undefined` through the interface stops with a `TypeError`.
            getters.push(super::getters::field_getter_some(
                cx, p.d, p.self_ty, i, ty, want,
            ));
            if let Some(setters) = setters.as_mut() {
                setters.push(SetterSrc::Method {
                    def: super::getters::some_setter(cx, p.d, p.self_ty, i, want),
                    d: p.d,
                    name: f.name.clone(),
                });
            }
        } else if let Some((g, set)) = accessor_slots(cx, p.d, p.self_ty, &f.name, want) {
            // A `get` (and `set`) accessor of the class provides the field (a `readonly` one
            // needs no `set`: its setter slot is used only by part writes, an error then).
            getters.push(g);
            let name = f.name.clone();
            let src = match set {
                Some(def) => Some(SetterSrc::Method { def, d: p.d, name }),
                None if f.readonly => Some(SetterSrc::GetterOnly {
                    d: p.d,
                    self_ty: p.self_ty,
                    name,
                    ty: want,
                }),
                None => None,
            };
            setters = setters.zip(src).map(|(mut v, src)| {
                v.push(src);
                v
            });
        } else {
            let tn = cx.display(want);
            let (cname, iname) = (&p.cname, &p.iname);
            cx.err(
                format!(
                    "`{cname}` must have a field `{}: {tn}` to implement `{iname}`",
                    f.name
                ),
                p.span,
            );
        }
    }
    (getters, setters)
}

/// The slots of field `name` (of type `want`) of an interface implemented by class `d`
/// through its accessors: the `get name()` (own or inherited), and the `set name(v)` when it
/// has one. Each dispatches virtually when it has a vtable slot, as a method does. `None`
/// when the class has no such getter.
pub(crate) fn accessor_slots(
    cx: &mut Ctx,
    d: DefId,
    self_ty: TyId,
    name: &str,
    want: TyId,
) -> Option<(DefId, Option<DefId>)> {
    // The class's own type parameters, or one instance's arguments.
    let self_args = match cx.ty.kind(self_ty) {
        crate::hir::TyKind::Adt(_, args) => args.clone(),
        _ => vec![],
    };
    let get = lookup_method(cx, d, &self_args, name)
        .filter(|f| matches!(f, Found::Class { .. }) && !f.is_static())
        .filter(|f| cx.fn_info(f.def()).is_getter)?;
    let def = get.def();
    let ret_ok = match super::ret_infer::is_pending(cx, def) {
        true => true,
        false => cx.subst(cx.fn_info(def).ret, &get.owner_args()) == want,
    };
    if !ret_ok {
        return None;
    }
    let getter = accessor_slot(cx, d, self_ty, &get, name, want);
    let key = crate::defs::member_key(name, true);
    let set = lookup_method(cx, d, &self_args, &key)
        .filter(|f| matches!(f, Found::Class { .. }) && !f.is_static())
        .filter(|f| {
            let info = cx.fn_info(f.def());
            let ps: Vec<TyId> = info.params.iter().map(|p| p.ty).collect();
            ps.len() == 1 && cx.subst(ps[0], &f.owner_args()) == want
        });
    let setter = set.map(|f| accessor_slot(cx, d, self_ty, &f, &key, cx.ty.unit));
    Some((getter, setter))
}

/// The impl entry of accessor `f` (method-table key `key`): virtual through a trampoline when
/// it has a vtable slot.
fn accessor_slot(cx: &mut Ctx, d: DefId, self_ty: TyId, f: &Found, key: &str, want: TyId) -> DefId {
    let def = f.def();
    cx.fn_info_mut(def).fixed_modes = true;
    match cx.adt(d).and_then(|a| a.vslots.get(key).copied()) {
        Some(slot) => trampoline(cx, d, self_ty, f, (slot, Some(want))),
        None => def,
    }
}

/// The implementation of interface method `m`: the class's own / inherited method (virtual
/// through a trampoline when it has a vtable slot), else the interface default.
fn impl_method(cx: &mut Ctx, p: &Pair, m: &IfaceMethod) -> Option<DefId> {
    let n = cx.adt(p.d).map_or(0, |a| a.generics.len());
    let self_args: Vec<TyId> = (0..n as u32).map(|i| cx.ty.param(i)).collect();
    let found = lookup_method(cx, p.d, &self_args, &m.name).filter(|f| !f.is_static());
    match found {
        Some(Found::Default { def, iface, .. }) if iface == p.bound.iface => Some(def),
        Some(f @ Found::Class { .. }) => {
            let def = f.def();
            let mut full_args = p.bound.args.clone();
            full_args.push(p.self_ty);
            let want = check_method_sig(cx, def, &f.owner_args(), m, &full_args, &p.iname);
            cx.fn_info_mut(def).fixed_modes = true;
            let slot = cx.adt(p.d).and_then(|a| a.vslots.get(&m.name).copied());
            Some(match slot {
                Some(slot) => trampoline(cx, p.d, p.self_ty, &f, (slot, want)),
                None => def,
            })
        }
        _ => {
            if let Some(def) = field_accessor(cx, p, m) {
                return Some(def);
            }
            if m.default.is_none() {
                let (cname, iname) = (&p.cname, &p.iname);
                cx.error(
                    Diagnostic::error(
                        format!(
                            "`{cname}` is missing method `{}` required by `{iname}`",
                            m.name
                        ),
                        p.span,
                    )
                    .with_label(m.span, "declared here"),
                );
            }
            m.default
        }
    }
}

/// The implementation of interface accessor `m` (`get v()` or `set v(x)`) by a field `v` of
/// the class of its type, as TypeScript allows (`class C implements T { v = 3 }`): reads or
/// writes the field.
fn field_accessor(cx: &mut Ctx, p: &Pair, m: &IfaceMethod) -> Option<DefId> {
    let (name, setter) = match m.name.strip_prefix("set ") {
        Some(n) => (n, true),
        None if m.is_getter => (m.name.as_str(), false),
        None => return None,
    };
    let mut full_args = p.bound.args.clone();
    full_args.push(p.self_ty);
    let want = match setter {
        true => cx.subst(m.params.first()?.ty, &full_args),
        false => cx.subst(m.ret, &full_args),
    };
    let fields = &cx.adt(p.d)?.fields;
    let i = fields.iter().position(|f| f.name == name)?;
    let ty = fields[i].ty;
    if !cx.same_ty(ty, want) {
        return None;
    }
    Some(match setter {
        true => {
            let borrowed = m.params.first()?.mode == crate::hir::PassMode::Borrow;
            super::getters::field_setter_taking(cx, p.d, p.self_ty, i, ty, borrowed)
        }
        false => super::getters::field_getter(cx, p.d, p.self_ty, i, ty),
    })
}

/// Does method `def` always mutate `this` (setters, `[Symbol.dispose]`)? Other methods start borrowing
/// `this`; mutation inference (`crate::ownership`) updates synthesized methods too.
fn mutates_this(cx: &Ctx, def: DefId) -> bool {
    cx.fn_info(def)
        .this
        .as_ref()
        .is_some_and(|t| t.mode == crate::hir::PassMode::BorrowMut)
}

/// A virtually dispatching impl entry for a method with a vtable slot (a subclass instance
/// boxed as the interface still runs its override); `want` is the interface method's result,
/// used while the method's own is still being inferred.
fn trampoline(
    cx: &mut Ctx,
    d: DefId,
    self_ty: TyId,
    f: &Found,
    (slot, want): (u32, Option<TyId>),
) -> DefId {
    let def = f.def();
    let owner_args = f.owner_args();
    let (name, params, ret) = {
        let info = cx.fn_info(def);
        let short = info.name.rsplit('.').next().unwrap_or("").to_string();
        (short, info.params.clone(), info.ret)
    };
    let params = params
        .into_iter()
        .map(|mut p| {
            p.ty = cx.subst(p.ty, &owner_args);
            p.default = None;
            p
        })
        .collect();
    let ret = match want.filter(|_| super::ret_infer::is_pending(cx, def)) {
        Some(want) => want,
        None => cx.subst(ret, &owner_args),
    };
    let qual = cx.adt(d).map(|a| a.qual_name.clone()).unwrap_or_default();
    let host = Host::adt(cx, d);
    synth_method(
        cx,
        &host,
        self_ty,
        format!("{qual}.<dyn {name}>"),
        params,
        ret,
        mutates_this(cx, def),
        Target::Slot(slot),
    )
}

/// Checks `def` against interface method `m`; returns `m`'s result type in the implementing
/// type's context (`None` after a mismatch of the type parameters).
pub(super) fn check_method_sig(
    cx: &mut Ctx,
    def: DefId,
    owner_args: &[TyId],
    m: &IfaceMethod,
    iface_args: &[TyId],
    iname: &str,
) -> Option<TyId> {
    let (ps, span, is_getter, is_generator) = {
        let f = cx.fn_info(def);
        (
            f.params.iter().map(|p| p.ty).collect::<Vec<_>>(),
            f.name_span,
            f.is_getter,
            f.is_generator,
        )
    };
    let iface_args = own_generics_match(cx, def, owner_args.len(), m, iface_args, iname)?;
    let iface_args = &iface_args[..];
    let ps: Vec<TyId> = ps.into_iter().map(|t| cx.subst(t, owner_args)).collect();
    let want_ps: Vec<TyId> = m
        .params
        .iter()
        .map(|p| cx.subst(p.ty, iface_args))
        .collect();
    let want_ret = cx.subst(m.ret, iface_args);
    let name = &m.name;
    let message = format!("method `{name}` has a different signature than required by `{iname}`");
    let ret_ok = if is_generator {
        // A generator's signature keeps its result with `E = never` and a written `E` as its
        // `throws` (collect/generator_sig.rs): compare the result as written. A generator's
        // result is always written, never inferred.
        let declared = cx.fn_info(def).declared_throws;
        if declared.is_none() {
            cx.iface_generators
                .push((def, format!("{iname}.{}", m.name)));
        }
        let e = declared.and_then(|t| t.ty);
        let ret = cx.fn_info(def).ret;
        let ret = cx.with_generator_error(ret, e.unwrap_or(cx.ty.never));
        {
            let r = cx.subst(ret, owner_args);
            cx.same_ty(r, want_ret)
        }
    } else if promise_results_unify(cx, def, owner_args, m, want_ret) {
        true
    } else {
        super::ret_infer::impl_ret(cx, (def, owner_args), want_ret, (span, message.clone()))
    };
    let ps_ok = cx.same_tys(&ps, &want_ps);
    if !ps_ok || !ret_ok {
        let mut d = Diagnostic::error(message, span);
        if is_generator && ps_ok {
            let want = cx.display(want_ret);
            d = d.with_note(format!("declare the generator's result as `{want}`: its error type is part of the result type"));
        } else if name == "return" && ps_ok && matches!(iname, "Iterator" | "AsyncIterator") {
            // `return(): void`: an iterator's early exit returns a finished result, as in TS.
            let t = iface_args.first().map_or("T".into(), |t| cx.display(*t));
            let want = match iname {
                "Iterator" => format!("return(): IteratorResult<{t}>"),
                _ => format!("async return(): Promise<IteratorResult<{t}>>"),
            };
            d = d.with_note(format!(
                "`return()` returns a finished result, as in TypeScript: declare it `{want}` and end it with `return {{ done: true }};`"
            ));
        } else if let Some(note) =
            crate::body::defaults::applied_default_note(cx, [(def, &ps, &want_ps); 2])
        {
            d = d.with_note(note);
        }
        cx.error(d);
    } else if is_getter != m.is_getter {
        let what = if m.is_getter { "a getter" } else { "a method" };
        cx.err(
            format!("`{name}` must be {what} to implement `{iname}.{name}`"),
            span,
        );
    }
    Some(want_ret)
}

/// A generic interface method is implemented by a method with as many own type params and the
/// same bounds on them. Returns `iface_args` extended so the interface method's own params map
/// onto the implementing method's (`None` after reporting a mismatch).
fn own_generics_match(
    cx: &mut Ctx,
    def: DefId,
    n_owner: usize,
    m: &IfaceMethod,
    iface_args: &[TyId],
    iname: &str,
) -> Option<Vec<TyId>> {
    let f = cx.fn_info(def);
    let (span, total) = (f.name_span, f.generics.len());
    let own_bounds: Vec<Vec<Bound>> = f
        .generics
        .bounds
        .get(n_owner..)
        .unwrap_or_default()
        .to_vec();
    let own = m.generics.len();
    let name = &m.name;
    if total.saturating_sub(n_owner) != own {
        cx.err(
            format!(
                "method `{name}` must have {own} type parameter(s) to implement `{iname}.{name}`"
            ),
            span,
        );
        return None;
    }
    let mut args = iface_args.to_vec();
    args.extend((0..own as u32).map(|k| cx.ty.param(n_owner as u32 + k)));
    let want: Vec<Vec<Bound>> = m
        .generics
        .bounds
        .iter()
        .map(|bs| {
            bs.iter()
                .map(|b| Bound {
                    iface: b.iface,
                    args: b.args.iter().map(|t| cx.subst(*t, &args)).collect(),
                })
                .collect()
        })
        .collect();
    if want != own_bounds {
        cx.err(
            format!(
                "the type parameters of `{name}` must have the same bounds as in `{iname}.{name}`"
            ),
            span,
        );
        return None;
    }
    Some(args)
}

/// A method returning a `Promise<T>` written without an error type implements an interface
/// method returning one: the two reject with what either is given (`throws::promise_sites`).
fn promise_results_unify(
    cx: &mut Ctx,
    def: DefId,
    owner_args: &[TyId],
    m: &IfaceMethod,
    want: TyId,
) -> bool {
    let (ret, ret_span) = (cx.fn_info(def).ret, cx.fn_info(def).ret_span);
    let have_site = ret_span.and_then(|s| cx.site_in(s, ret));
    let have = cx.subst(ret, owner_args);
    // The interface method's promise error is its `throws` (`collect::sigs::promise_throws`).
    let want = match (cx.ty.promise_payload(want), m.throws.and_then(|t| t.ty)) {
        (Some(v), Some(e)) => cx.ty.promise_rejecting(v, e),
        _ => want,
    };
    if have_site.is_some() && have == want {
        return true;
    }
    let want_site = cx.written_promise_site(m.span, want);
    cx.unify_promise_results((have, have_site), (want, want_site))
}
