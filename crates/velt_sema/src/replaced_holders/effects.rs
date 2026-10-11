//! What running an expression may do: the assignments, calls and function values it reaches.

use super::{field_keys, fn_or_null, has_params, member, slot_of, Effects, FnValue, Frame, Scope};
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Callee, DefId, Expr, ExprKind as E, TyKind};

impl Effects {
    pub(super) fn runs_code(&self) -> bool {
        !self.assigns.is_empty()
            || !self.generic.is_empty()
            || !self.calls.is_empty()
            || self.indirect
            || self.own
            || !self.methods.is_empty()
            || self.anything
    }

    pub(super) fn note(
        &mut self,
        cx: &Ctx,
        slots: &[Option<Box<[bool]>>],
        scope: &Scope,
        e: &Expr,
    ) {
        match &e.kind {
            // Only a field holds an object literal inline (an element replaced is #820).
            E::Assign { place, .. } => match &place.kind {
                E::Field { .. } if has_params(cx, place.ty) => self.generic.push(place.ty),
                E::Field { base, index, .. } => {
                    self.assigns.insert(place.ty);
                    match field_keys(cx, base.ty, *index, place.ty) {
                        Some((_, loose)) if has_params(cx, base.ty) => {
                            self.loose.insert(loose);
                        }
                        Some((key, _)) => {
                            self.fields.insert(key);
                        }
                        None => {}
                    }
                }
                _ => {
                    self.assigns.insert(place.ty);
                }
            },
            E::Call { callee, args } => match callee {
                Callee::Def(d, targs) => {
                    self.callbacks(cx, scope, args, slot_of(slots, *d));
                    self.calls.push((*d, Frame::Known(targs.clone())));
                    self.type_args.extend(targs);
                }
                Callee::Intrinsic(_) => self.callbacks(cx, scope, args, None),
                Callee::Indirect(f) => {
                    self.callbacks(cx, scope, args, None);
                    self.run(scope.classify(cx, f));
                }
                Callee::Virtual { slot } => {
                    self.callbacks(cx, scope, args, None);
                    let m = args.first().and_then(|r| match cx.ty.kind(r.ty) {
                        TyKind::Adt(d, _) => match cx.info.get(d.0 as usize) {
                            Some(DefInfo::Adt(a)) => a.vtable.get(*slot as usize).copied(),
                            _ => None,
                        },
                        _ => None,
                    });
                    let name = m.and_then(|m| match cx.info.get(m.0 as usize) {
                        Some(DefInfo::Fn(f)) => Some(member(&f.name).to_string()),
                        _ => None,
                    });
                    self.methods.push(name);
                }
                Callee::Dyn { slot } => {
                    self.callbacks(cx, scope, args, None);
                    let iface = args.first().and_then(|r| match cx.ty.kind(r.ty) {
                        TyKind::Dyn(i, _) => Some(*i),
                        _ => None,
                    });
                    self.iface_call(cx, iface, *slot);
                }
                Callee::ParamMethod {
                    iface,
                    iface_args,
                    slot,
                    method_type_args,
                } => {
                    self.callbacks(cx, scope, args, None);
                    self.type_args.extend(iface_args);
                    self.type_args.extend(method_type_args);
                    self.iface_call(cx, Some(*iface), *slot);
                }
            },
            E::New {
                def,
                type_args,
                args,
            } => {
                self.type_args.extend(type_args);
                match cx.info.get(def.0 as usize) {
                    Some(DefInfo::Adt(a)) => {
                        let ctor = a.ctor;
                        let s = ctor.and_then(|c| slot_of(slots, c));
                        self.callbacks(cx, scope, args, s);
                        self.calls
                            .extend(ctor.map(|c| (c, Frame::Known(type_args.clone()))));
                    }
                    _ => {
                        self.callbacks(cx, scope, args, None);
                        self.anything = true;
                    }
                }
            }
            E::Closure(d) => self.values.push(*d),
            E::FnRef(d, targs) => {
                self.values.push(*d);
                self.type_args.extend(targs);
            }
            E::Await(_) => self.anything = true,
            _ => {}
        }
    }

    /// Running (or handing on) the function value `v`.
    fn run(&mut self, v: FnValue) {
        match v {
            FnValue::Own => self.own = true,
            FnValue::Known(d, f) => self.calls.push((d, f)),
            FnValue::Other => self.indirect = true,
        }
    }

    /// The function values among a call's arguments: a function an argument names runs (it is
    /// followed), the function's own parameter is its callers' business, and anything else
    /// may be any function value. `slots`: which of the callee's parameters take a function
    /// value (aligned with the arguments at the end, past a receiver passed apart); there an
    /// argument of any type counts.
    fn callbacks(&mut self, cx: &Ctx, scope: &Scope, args: &[Expr], slots: Option<&[bool]>) {
        let skip = slots.map_or(0, |s| s.len() as isize - args.len() as isize);
        for (i, a) in args.iter().enumerate() {
            let slot = slots.is_some_and(|s| {
                skip < 0 || s.get((i as isize + skip) as usize).copied().unwrap_or(true)
            });
            if slot || fn_or_null(cx, a.ty) {
                self.run(scope.classify(cx, a));
            }
        }
    }

    /// A call of method `slot` of interface `iface`: each implementation and the default, and
    /// the methods of its name (overrides of a class's implementation).
    fn iface_call(&mut self, cx: &Ctx, iface: Option<DefId>, slot: u32) {
        let Some(iface) = iface else {
            self.methods.push(None);
            return;
        };
        for imp in cx.impls.iter().filter(|i| i.iface == iface) {
            if let Some(&m) = imp.methods.get(slot as usize) {
                self.calls.push((m, Frame::Unknown));
            }
        }
        let name = match cx.info.get(iface.0 as usize) {
            Some(DefInfo::Iface(i)) => i.methods.get(slot as usize).map(|m| {
                if let Some(d) = m.default {
                    self.calls.push((d, Frame::Unknown));
                }
                m.name.clone()
            }),
            _ => None,
        };
        self.methods.push(name);
    }
}
