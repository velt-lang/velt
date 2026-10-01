//! Phase 3b: interface inheritance (`interface B extends A<X>`), flattened so that no later stage
//! needs to know about it:
//! - `B`'s fields and method slots are its own followed by every inherited one (substituted with
//!   the `extends` args; an interface reached twice, diamond-style, contributes once). A method
//!   `B` redeclares keeps `B`'s slot and must have the inherited signature.
//! - An inherited default method becomes a synthesized default of `B` ([`forwarder`]) calling
//!   `A`'s default, so each interface's defaults are generic over its own params plus `Self`.
//! - `IfaceInfo::parents` lists every ancestor (transitively), and a class implementing `B`
//!   also implements each ancestor (own `ImplDef`s): `T extends B` satisfies `A` bounds, and a
//!   `B` implementor converts to an `A` value.

use velt_common::{Diagnostic, Span};

use super::forwarders::{synth_method, Host, Target};
use crate::ctx::Ctx;
use crate::defs::{Bound, FieldInfo, IfaceMethod};
use crate::hir::{DefId, TyId};

pub(super) fn flatten_all(cx: &mut Ctx) {
    let ifaces: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| cx.iface(*d).is_some_and(|i| i.decl.is_some()))
        .collect();
    let mut done = vec![false; cx.info.len()];
    for &d in &ifaces {
        flatten(cx, d, &mut done, &mut vec![]);
    }
    inherit_implements(cx);
}

/// Flatten `d` after its parents (depth-first; `stack` detects cycles).
fn flatten(cx: &mut Ctx, d: DefId, done: &mut Vec<bool>, stack: &mut Vec<DefId>) {
    if done[d.0 as usize] {
        return;
    }
    let parents = cx.iface(d).expect("ICE: iface").parents.clone();
    stack.push(d);
    let mut ok_parents = vec![];
    for p in parents {
        if stack.contains(&p.iface) {
            let span = cx.iface(d).map_or(Span::DUMMY, |i| i.span);
            cx.err("interface inheritance cycle", span);
            continue;
        }
        flatten(cx, p.iface, done, stack);
        ok_parents.push(p);
    }
    stack.pop();
    let mut ancestors: Vec<Bound> = vec![];
    for p in &ok_parents {
        inherit_from(cx, d, p);
        let grand = cx.iface(p.iface).expect("ICE: iface").parents.clone();
        let substituted = grand.into_iter().map(|g| subst_bound(cx, &g, &p.args));
        for a in std::iter::once(p.clone()).chain(substituted) {
            if !ancestors.contains(&a) {
                ancestors.push(a);
            }
        }
    }
    iface_mut(cx, d).parents = ancestors;
    done[d.0 as usize] = true;
}

fn iface_mut<'a, 'm>(cx: &'a mut Ctx<'m>, d: DefId) -> &'a mut crate::defs::IfaceInfo<'m> {
    match &mut cx.info[d.0 as usize] {
        crate::defs::DefInfo::Iface(i) => i,
        _ => panic!("ICE: def {d:?} is not an interface"),
    }
}

fn subst_bound(cx: &mut Ctx, b: &Bound, args: &[TyId]) -> Bound {
    Bound {
        iface: b.iface,
        args: b.args.iter().map(|t| cx.ty.subst(*t, args)).collect(),
    }
}

/// Append parent `p`'s (already flattened) fields and methods to `d`.
fn inherit_from(cx: &mut Ctx, d: DefId, p: &Bound) {
    let parent = cx.iface(p.iface).expect("ICE: iface");
    let (pfields, pmethods, pname) = (
        parent.fields.clone(),
        parent.methods.clone(),
        parent.name.clone(),
    );
    for mut f in pfields {
        f.ty = cx.ty.subst(f.ty, &p.args);
        inherit_field(cx, d, f, &pname);
    }
    let n = cx.iface(d).expect("ICE: iface").generics.len() as u32;
    // Parent methods are in terms of the parent's params plus its `Self` (= `d`'s `Self`).
    let mut args = p.args.clone();
    args.push(cx.ty.param(n));
    for mut m in pmethods {
        // A generic method's own params follow `Self`: renumber them after `d`'s `Self`.
        let mut margs = args.clone();
        margs.extend((0..m.generics.len() as u32).map(|k| cx.ty.param(n + 1 + k)));
        for bs in &mut m.generics.bounds {
            for b in bs.iter_mut() {
                *b = subst_bound(cx, b, &margs);
            }
        }
        inherit_method(cx, d, m, &margs, &pname);
    }
}

fn inherit_field(cx: &mut Ctx, d: DefId, f: FieldInfo, pname: &str) {
    let i = cx.iface(d).expect("ICE: iface");
    match i.fields.iter().find(|g| g.name == f.name) {
        Some(g) if g.ty == f.ty => {}
        Some(g) => {
            let span = g.span;
            cx.err(
                format!(
                    "field `{}` conflicts with the field inherited from `{pname}`",
                    f.name
                ),
                span,
            );
        }
        None => iface_mut(cx, d).fields.push(f),
    }
}

fn inherit_method(cx: &mut Ctx, d: DefId, m: IfaceMethod, args: &[TyId], pname: &str) {
    let params: Vec<_> = m
        .params
        .iter()
        .map(|q| {
            let mut q = q.clone();
            q.ty = cx.ty.subst(q.ty, args);
            q
        })
        .collect();
    let ret = cx.ty.subst(m.ret, args);
    let existing = cx
        .iface(d)
        .expect("ICE: iface")
        .methods
        .iter()
        .find(|x| x.name == m.name)
        .cloned();
    if let Some(own) = existing {
        let same = own.ret == ret
            && own.is_getter == m.is_getter
            && own
                .params
                .iter()
                .map(|q| q.ty)
                .eq(params.iter().map(|q| q.ty));
        if !same {
            cx.error(
                Diagnostic::error(
                    format!(
                        "method `{}` must have the same signature as in `{pname}`",
                        m.name
                    ),
                    own.span,
                )
                .with_label(m.span, "inherited from here"),
            );
        }
        return;
    }
    let default = m
        .default
        .map(|def| forwarder(cx, d, &m, def, args, &params, ret));
    iface_mut(cx, d).methods.push(IfaceMethod {
        params,
        ret,
        default,
        ..m
    });
}

/// `B.m(this, ..)` calling the parent's default `def` with `Self = B`'s `Self`.
#[allow(clippy::too_many_arguments)] // the inherited method and its substituted signature
fn forwarder(
    cx: &mut Ctx,
    d: DefId,
    m: &IfaceMethod,
    def: DefId,
    args: &[TyId],
    params: &[crate::defs::ParamSig],
    ret: TyId,
) -> DefId {
    let i = cx.iface(d).expect("ICE: iface");
    let (qual, span, module) = (i.qual_name.clone(), i.span, i.module);
    let mut generics = i.generics.clone();
    let n = generics.len() as u32;
    generics.push("Self");
    let own_args = (0..n).map(|k| cx.ty.param(k)).collect();
    generics.bounds[n as usize].push(Bound {
        iface: d,
        args: own_args,
    });
    let host = Host {
        d,
        generics,
        span,
        module,
    };
    let self_ty = cx.ty.param(n);
    let params = params
        .iter()
        .map(|q| crate::defs::ParamSig {
            default: None,
            ..q.clone()
        })
        .collect();
    let target = Target::Default(def, args.to_vec());
    let name = format!("{qual}.{}", m.name);
    let fwd = synth_method(cx, &host, self_ty, name, params, ret, m.mut_this, target);
    cx.fn_info_mut(fwd).is_getter = m.is_getter;
    fwd
}

/// A type implementing `B` implements every ancestor of `B` too.
fn inherit_implements(cx: &mut Ctx) {
    let adts: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| cx.adt(*d).is_some_and(|a| !a.implements.is_empty()))
        .collect();
    for d in adts {
        let mut all = cx.adt(d).expect("ICE: adt").implements.clone();
        let direct = all.clone();
        for b in direct {
            let ancestors = cx
                .iface(b.iface)
                .map(|i| i.parents.clone())
                .unwrap_or_default();
            for a in ancestors {
                let a = subst_bound(cx, &a, &b.args);
                if !all.contains(&a) {
                    all.push(a);
                }
            }
        }
        cx.adt_mut(d).implements = all;
    }
}
