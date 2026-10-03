//! Phase 5: `implements` checking. For every `class/struct C implements I<args>` the class must
//! have I's fields (same name and type) and a method for every interface method without a
//! default (same parameter and return types). Each pair becomes a `hir::ImplDef` whose
//! `methods` list the implementation per interface slot (own, inherited, or I's default),
//! followed by one synthesized getter per interface field ([`getters`](super::getters)).

use std::collections::HashMap;

use velt_common::Diagnostic;

use super::forwarders::{synth_method, Host, Target};
use super::lookup::{lookup_method, Found};
use super::shapes::self_type;
use crate::ctx::Ctx;
use crate::defs::{Bound, IfaceMethod};
use crate::hir::{DefId, ImplDef, TyId};

pub(super) fn build_impls(cx: &mut Ctx) {
    let adts: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| cx.adt(*d).is_some_and(|a| !a.implements.is_empty()))
        .collect();
    let mut getters = Getters::new();
    for d in adts {
        let implements = cx.adt(d).expect("ICE: adt").implements.clone();
        for b in implements {
            if let Some(imp) = check_impl(cx, d, &b, &mut getters) {
                cx.impls.push(imp);
            }
        }
    }
}

/// (type, field index) → its getter, shared by every interface that declares the field (an
/// interface and the ones it extends).
type Getters = HashMap<(DefId, usize), DefId>;

/// One `C implements I<args>` being checked.
struct Pair<'b> {
    d: DefId,
    self_ty: TyId,
    bound: &'b Bound,
    cname: String,
    iname: String,
    span: velt_common::Span,
}

fn check_impl(cx: &mut Ctx, d: DefId, b: &Bound, cache: &mut Getters) -> Option<ImplDef> {
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
    let getters = field_getters(cx, &pair, cache);
    let mut methods = vec![];
    for m in &imethods {
        methods.push(impl_method(cx, &pair, m)?);
    }
    methods.extend(getters);
    Some(ImplDef {
        ty: self_ty,
        generics: n as u32,
        iface: b.iface,
        iface_args: b.args.clone(),
        methods,
    })
}

/// The class must have every interface field (same type); each gets a getter.
fn field_getters(cx: &mut Ctx, p: &Pair, cache: &mut Getters) -> Vec<DefId> {
    let ifields = cx
        .iface(p.bound.iface)
        .map(|i| i.fields.clone())
        .unwrap_or_default();
    let mut getters = vec![];
    for f in ifields {
        let want = cx.ty.subst(f.ty, &p.bound.args);
        let have = cx
            .adt(p.d)
            .and_then(|a| a.fields.iter().position(|g| g.name == f.name))
            .map(|i| (i, cx.adt(p.d).expect("ICE: adt").fields[i].ty));
        if let Some((i, ty)) = have.filter(|(_, ty)| *ty == want) {
            let g = match cache.get(&(p.d, i)) {
                Some(&g) => g,
                None => super::getters::field_getter(cx, p.d, p.self_ty, i, ty),
            };
            cache.insert((p.d, i), g);
            getters.push(g);
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
    getters
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
            p.ty = cx.ty.subst(p.ty, &owner_args);
            p.default = None;
            p
        })
        .collect();
    let ret = match want.filter(|_| super::ret_infer::is_pending(cx, def)) {
        Some(want) => want,
        None => cx.ty.subst(ret, &owner_args),
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
fn check_method_sig(
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
    let ps: Vec<TyId> = ps.into_iter().map(|t| cx.ty.subst(t, owner_args)).collect();
    let want_ps: Vec<TyId> = m
        .params
        .iter()
        .map(|p| cx.ty.subst(p.ty, iface_args))
        .collect();
    let want_ret = cx.ty.subst(m.ret, iface_args);
    let name = &m.name;
    let message = format!("method `{name}` has a different signature than required by `{iname}`");
    let ret_ok = if is_generator {
        // A generator's signature keeps its result with `E = never` and a written `E` as its
        // `throws` (collect/generator_sig.rs): compare the result as written. A generator's
        // result is always written, never inferred.
        let e = cx.fn_info(def).declared_throws.and_then(|t| t.ty);
        let ret = cx.fn_info(def).ret;
        let ret = cx.with_generator_error(ret, e.unwrap_or(cx.ty.never));
        cx.ty.subst(ret, owner_args) == want_ret
    } else {
        super::ret_infer::impl_ret(cx, (def, owner_args), want_ret, (span, message.clone()))
    };
    if ps != want_ps || !ret_ok {
        let mut d = Diagnostic::error(message, span);
        if is_generator && ps == want_ps {
            let want = cx.display(want_ret);
            d = d.with_note(format!("declare the generator's result as `{want}`: its error type is part of the result type"));
        } else if name == "return" && ps == want_ps && matches!(iname, "Iterator" | "AsyncIterator")
        {
            // `return(): void`: an iterator's early exit returns a finished result, as in TS.
            let t = iface_args.first().map_or("T".into(), |t| cx.display(*t));
            let want = match iname {
                "Iterator" => format!("return(): IteratorResult<{t}>"),
                _ => format!("async return(): Promise<IteratorResult<{t}>>"),
            };
            d = d.with_note(format!(
                "`return()` returns a finished result, as in TypeScript: declare it `{want}` and end it with `return {{ done: true }};`"
            ));
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
                    args: b.args.iter().map(|t| cx.ty.subst(*t, &args)).collect(),
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
