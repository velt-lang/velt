//! Checked conversions from `any` / `unknown` to a generic interface at other type arguments
//! (#1185): `x as Ctx<any, any>` where `x` holds a value implementing `Ctx<P, S>` (an object
//! literal's struct, a class instance). Node keeps the object; Velt wraps it in the adapter from
//! `Ctx<P, S>` to `Ctx<any, any>` (`iface_adapt`), chosen at run time by the value's type.
//!
//! The conversions are recorded while bodies are checked ([`FnCx::note_dynamic_iface`]); once
//! every body is checked, and so every implementation of the interface exists,
//! [`make_dynamic_iface_adapters`] makes the adapter from each implementation's arguments to
//! each recorded target. Lowering finds them by their wrapped type and implemented arguments
//! (velt_vir `any_iface.rs`). A program without such a conversion is unchanged.

use velt_common::Span;

use crate::body::{FnCx, Frame};
use crate::ctx::Ctx;
use crate::defs::FnKind;
use crate::hir::{DefId, TyId, TyKind};
use crate::resolve::TyEnv;

/// A recorded conversion target: the interface, its type arguments, and where it was written.
pub(crate) type DynIfaceTarget = (DefId, Vec<TyId>, usize, Span);

impl FnCx<'_, '_> {
    /// Record a checked conversion of a dynamic value to `exp` (or `exp | null`), when it is a
    /// generic interface type other than a dynamic one.
    pub(super) fn note_dynamic_iface(&mut self, exp: TyId, span: Span) {
        let t = self.cx.ty.opt_payload(exp).unwrap_or(exp);
        let TyKind::Dyn(iface, args) = self.cx.ty.kind(t).clone() else {
            return;
        };
        if args.is_empty() || self.cx.dynamic(t).is_some() {
            return;
        }
        let mut known = self.cx.dyn_iface_targets.iter();
        if known.any(|(i, a, ..)| *i == iface && *a == args) {
            return;
        }
        self.cx
            .dyn_iface_targets
            .push((iface, args, self.module, span));
    }
}

/// Make the adapters from every implementation of a recorded target's interface (at other,
/// concrete type arguments) to the target's arguments.
pub(crate) fn make_dynamic_iface_adapters(cx: &mut Ctx) {
    let mut done = 0;
    while done < cx.dyn_iface_targets.len() {
        let (iface, to, module, span) = cx.dyn_iface_targets[done].clone();
        done += 1;
        let adapters: Vec<u32> = cx.iface_adapters.values().map(|a| a.impl_index).collect();
        let mut froms: Vec<Vec<TyId>> = vec![];
        let impls: Vec<(u32, TyId, Vec<TyId>)> = (cx.impls.iter().enumerate())
            .filter(|(k, i)| i.iface == iface && !adapters.contains(&(*k as u32)))
            .map(|(_, i)| (i.generics, i.ty, i.iface_args.clone()))
            .collect();
        for (generics, self_ty, args) in impls {
            let instances = match generics {
                0 => vec![args],
                _ => (generic_instances(cx, self_ty, generics).into_iter())
                    .map(|xs| args.iter().map(|a| cx.subst(*a, &xs)).collect())
                    .collect(),
            };
            for from in instances {
                if from != to && !froms.contains(&from) {
                    froms.push(from);
                }
            }
        }
        for from in froms {
            let env = TyEnv::new(module, &[]);
            let mut g = FnCx::new(cx, module, env, Frame::new(FnKind::Free, None));
            if g.iface_adapts(iface, &from, &to) {
                g.iface_adapter(iface, &from, &to, span);
            }
        }
    }
}

/// The type arguments of the concrete instances of the generic class `self_ty` (`G<T0, …>`,
/// `generics` parameters in order) the program names: what a generic implementation's values
/// are at run time.
pub(crate) fn generic_instances(cx: &Ctx, self_ty: TyId, generics: u32) -> Vec<Vec<TyId>> {
    let TyKind::Adt(d, params) = cx.ty.kind(self_ty) else {
        return vec![];
    };
    let identity = params.len() == generics as usize
        && (params.iter().enumerate())
            .all(|(i, p)| matches!(cx.ty.kind(*p), TyKind::Param(n) if *n == i as u32));
    if !identity {
        return vec![];
    }
    let d = *d;
    let mut out = vec![];
    for k in 0..cx.ty.table.len() as u32 {
        let TyKind::Adt(e, args) = cx.ty.kind(TyId(k)) else {
            continue;
        };
        if *e != d || args.len() != generics as usize {
            continue;
        }
        let mut open = false;
        for a in args {
            cx.ty.visit(*a, &mut |k| {
                open |= matches!(k, TyKind::Param(_) | TyKind::Error | TyKind::Op(..))
            });
        }
        if !open {
            out.push(args.clone());
        }
    }
    out
}
