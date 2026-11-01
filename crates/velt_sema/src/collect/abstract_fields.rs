//! Fields implementing abstract members, as TypeScript allows: `readonly name = "q"` for
//! `abstract readonly name: string` (or an abstract `get name()`), `count = 3` for `abstract
//! count: number` (its getter and setter), `run: () => i64 = …` for `abstract run(): i64`. The
//! class gets methods reading, writing or calling the field, which fill the abstract members'
//! vtable slots.

use velt_common::{Diagnostic, Span};

use super::abstracts::implements_abstract;
use super::forwarders::{synth_method, Host, Target};
use super::getters::{field_getter, field_setter};
use super::lookup::{lookup_method, Found};
use crate::ctx::Ctx;
use crate::defs::{member_key, MethodRef};
use crate::hir::{DefId, TyId, TyKind};

/// The abstract members class `d` implements with fields: (method key, field index).
pub(super) fn field_impls(cx: &Ctx, d: DefId) -> Vec<(String, usize)> {
    let Some(a) = cx.adt(d) else { return vec![] };
    let mut out = vec![];
    for (i, f) in a.fields.iter().enumerate() {
        for setter in [false, true] {
            let key = member_key(&f.name, setter);
            let own = a.methods.get(&key).is_some_and(|m| !m.is_static);
            if !own && implements_abstract(cx, d, &key) {
                out.push((key, i));
            }
        }
    }
    out
}

/// The methods implementing [`field_impls`] for class `d`, registered as its own methods:
/// (method key, method) for its vtable.
pub(super) fn field_methods(cx: &mut Ctx, d: DefId) -> Vec<(String, DefId)> {
    let impls = field_impls(cx, d);
    if impls.is_empty() {
        return vec![];
    }
    let n = cx.adt(d).expect("ICE: adt").generics.len();
    let self_args: Vec<TyId> = (0..n as u32).map(|i| cx.ty.param(i)).collect();
    let self_ty = cx.ty.intern(TyKind::Adt(d, self_args));
    let mut out = vec![];
    let mut reported = vec![];
    for (key, i) in impls {
        if reported.contains(&i) {
            continue;
        }
        let Some(base) = abstract_member(cx, d, &key) else {
            continue;
        };
        let f = &cx.adt(d).expect("ICE: adt").fields[i];
        let (fty, fname, fspan) = (f.ty, f.name.clone(), f.span);
        let args = base.owner_args();
        let info = cx.fn_info(base.def());
        let (is_getter, ret) = (info.is_getter, info.ret);
        let params = info.params.clone();
        let def = if key.starts_with("set ") {
            let want = cx.subst(params[0].ty, &args);
            mismatch(cx, fty, want, &fname, fspan, "property")
                .then(|| field_setter(cx, d, self_ty, i, fty))
        } else if is_getter {
            let want = cx.subst(ret, &args);
            mismatch(cx, fty, want, &fname, fspan, "property")
                .then(|| field_getter(cx, d, self_ty, i, fty))
        } else {
            let params: Vec<_> = (params.into_iter())
                .map(|mut p| {
                    p.ty = cx.subst(p.ty, &args);
                    p
                })
                .collect();
            let ret = cx.subst(ret, &args);
            let pts: Vec<TyId> = params.iter().map(|p| p.ty).collect();
            let ok = match cx.ty.kind(fty).clone() {
                TyKind::FnPtr {
                    params: fp,
                    ret: fr,
                    ..
                } => cx.same_tys(&fp, &pts) && cx.same_ty(fr, ret),
                _ => false,
            };
            if !ok {
                method_mismatch(cx, &fname, fspan);
            }
            ok.then(|| {
                let qual = cx.adt(d).expect("ICE: adt").qual_name.clone();
                let host = Host::adt(cx, d);
                let target = Target::Field {
                    index: i as u32,
                    fn_ty: fty,
                    this: None,
                };
                let name = format!("{qual}.{fname}");
                synth_method(cx, &host, self_ty, name, params, ret, false, target)
            })
        };
        let Some(def) = def else {
            reported.push(i);
            continue;
        };
        let r = MethodRef {
            def,
            is_static: false,
        };
        cx.adt_mut(d).methods.insert(key.clone(), r);
        out.push((key, def));
    }
    out
}

/// The abstract member `key` class `d` inherits.
fn abstract_member(cx: &mut Ctx, d: DefId, key: &str) -> Option<Found> {
    let base = cx.adt(d)?.base?;
    let (b, bargs) = cx.class_of(base)?;
    lookup_method(cx, b, &bargs, key).filter(|f| !f.is_static())
}

/// Is field type `ty` the abstract property's type `want`? Reported when not (TS2416).
fn mismatch(cx: &mut Ctx, ty: TyId, want: TyId, name: &str, span: Span, what: &str) -> bool {
    if cx.same_ty(ty, want) {
        return true;
    }
    let want = cx.display(want);
    cx.error(Diagnostic::error(
        format!("field `{name}` does not have the type of the abstract {what} it implements, `{want}` (TypeScript's TS2416)"),
        span,
    ));
    false
}

fn method_mismatch(cx: &mut Ctx, name: &str, span: Span) {
    cx.error(
        Diagnostic::error(
            format!("field `{name}` does not have the type of the abstract method it implements (TypeScript's TS2416)"),
            span,
        )
        .with_note(format!(
            "declare its function type, with the method's parameters and result (`{name}: (…) => T = …`)"
        )),
    );
}
