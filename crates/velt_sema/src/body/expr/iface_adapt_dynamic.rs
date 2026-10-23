//! Checked conversions from `any` / `unknown` to a generic interface at other type arguments
//! (#1185): `x as Ctx<any, any>` where `x` holds a value implementing `Ctx<P, S>` (an object
//! literal's struct, a class instance, an instance of a generic class). Node keeps the object;
//! Velt wraps it in an adapter to `Ctx<any, any>`, chosen at run time by the value's type.
//!
//! The values such an `any` may hold are not all known here: a generic function makes
//! instances of a generic class that only lowering sees (`new G<T>(x)` in `mk<T>`). So one
//! adapter is made per recorded target, generic over the arguments it adapts from: the
//! adapter to `Ctx<any, number>` wraps a `Ctx<T0, number>`, and its instance at `T0 = string`
//! is the adapter from `Ctx<string, number>`. Each slot converts between `T0` and `any` as a
//! generic function does (boxing, a checked conversion out of `any`).
//!
//! The conversions are recorded while bodies are checked ([`FnCx::note_dynamic_iface`]); once
//! every body is checked, [`make_dynamic_iface_adapters`] makes the adapter of each target.
//! Lowering instantiates it for each type an `any` holds that implements the interface at
//! other arguments (velt_vir `any_iface.rs`). A program without such a conversion is unchanged.

use velt_common::Span;

use super::iface_adapt::{adaptable, is_dynamic_arg, Adapter, Mode};
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

    /// The generic adapter to `iface<to>` (module docs), made on first use: over one type
    /// parameter per dynamic argument of `to`. `None` when `to` has none or a slot does not
    /// convert.
    pub(super) fn dynamic_adapter(
        &mut self,
        iface: DefId,
        to: &[TyId],
        span: Span,
    ) -> Option<Adapter> {
        let mut from = to.to_vec();
        let mut n = 0;
        for (i, t) in to.iter().enumerate() {
            if is_dynamic_arg(self.cx, *t) {
                from[i] = self.cx.ty.param(n);
                n += 1;
            }
        }
        if n == 0 || !adaptable(self.cx, iface, &from, to, &mut Mode::generic()) {
            return None;
        }
        Some(self.iface_adapter(iface, &from, to, n, span))
    }

    /// The generic adapter to `iface<to>` and its type arguments for a value of `iface<from>`:
    /// `from`'s arguments where `to`'s are dynamic, the others the same as `to`'s.
    pub(super) fn dynamic_adapter_at(
        &mut self,
        iface: DefId,
        from: &[TyId],
        to: &[TyId],
        span: Span,
    ) -> Option<(Adapter, Vec<TyId>)> {
        if from.len() != to.len() {
            return None;
        }
        let mut args = vec![];
        for (a, b) in from.iter().zip(to) {
            match is_dynamic_arg(self.cx, *b) {
                true => args.push(*a),
                false if a == b => {}
                false => return None,
            }
        }
        let a = self.dynamic_adapter(iface, to, span)?;
        Some((a, args))
    }
}

/// Make the adapter of every recorded target (module docs).
pub(crate) fn make_dynamic_iface_adapters(cx: &mut Ctx) {
    let mut done = 0;
    while done < cx.dyn_iface_targets.len() {
        let (iface, to, module, span) = cx.dyn_iface_targets[done].clone();
        done += 1;
        let env = TyEnv::new(module, &[]);
        let mut g = FnCx::new(cx, module, env, Frame::new(FnKind::Free, None));
        g.dynamic_adapter(iface, &to, span);
    }
}
