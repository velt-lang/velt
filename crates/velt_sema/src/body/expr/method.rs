//! Method resolution on a receiver type. Lookup order: the type's own methods (classes: then
//! base classes), default methods of interfaces it implements, interface methods (receivers of
//! interface type or bounded generic type), `extend` blocks whose target matches, then the
//! builtin methods (`push`/`pop` on arrays, `clone` everywhere).
//!
//! Dispatch: a class method with a vtable slot in the receiver's static class is
//! `Callee::Virtual`; everything else on concrete types is a direct `Callee::Def`.

use crate::body::FnCx;
use crate::collect::lookup_method;
use crate::defs::{Bound, IfaceMethod};
use crate::hir::{DefId, PassMode, TyId, TyKind};

/// Builtin methods implemented by intrinsics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuiltinMethod {
    Push,
    Pop,
    Clone,
    ToString,
    /// Atomics on `shared<i64>` (and other 64-bit ints).
    SharedAdd,
    SharedGet,
    SharedSet,
    /// `with(f)` on `Mutex<T>` / `shared<Mutex<T>>`.
    MutexWith,
}

pub(crate) enum Resolved {
    /// Direct call. `slots` = owner type args (known) followed by the method's own (unknown);
    /// the receiver is converted to `recv_ty` (upcast to the declaring class if inherited).
    Def {
        def: DefId,
        slots: Vec<Option<TyId>>,
        recv_ty: TyId,
        is_static: bool,
    },
    /// Vtable call on a class receiver (`slots`: the declaring class's args).
    Virtual {
        def: DefId,
        slot: u32,
        slots: Vec<Option<TyId>>,
    },
    /// Interface method on an interface value (`Dyn`) or bounded generic (`Param`).
    Iface {
        on_param: bool,
        iface: DefId,
        iface_args: Vec<TyId>,
        slot: u32,
        method: IfaceMethod,
    },
    Builtin(BuiltinMethod),
}

impl FnCx<'_, '_> {
    pub(crate) fn resolve_method(&mut self, recv: TyId, name: &str) -> Option<Resolved> {
        if let Some(r) = self.own_method(recv, name) {
            return Some(r);
        }
        if let Some(r) = self.iface_method(recv, name) {
            return Some(r);
        }
        if let Some(r) = self.extension_method(recv, name) {
            return Some(r);
        }
        self.builtin_method(recv, name).map(Resolved::Builtin)
    }

    pub(crate) fn method_exists(&mut self, recv: TyId, name: &str) -> bool {
        self.resolve_method(recv, name).is_some()
    }

    fn own_generic_slots(&self, def: DefId, owner_args: &[TyId]) -> Vec<Option<TyId>> {
        let n = self.cx.fn_info(def).generics.len();
        let mut slots: Vec<Option<TyId>> = owner_args.iter().map(|t| Some(*t)).collect();
        slots.resize(n.max(owner_args.len()), None);
        slots
    }

    /// Methods declared on the receiver's struct/class (or a base class), and defaults of
    /// interfaces they implement. A class receiver whose static class has a vtable slot for the
    /// method dispatches virtually.
    fn own_method(&mut self, recv: TyId, name: &str) -> Option<Resolved> {
        let (d, args) = self.adt_of(recv)?;
        let found = lookup_method(self.cx, d, &args, name)?;
        let slots = self.own_generic_slots(found.def(), &found.owner_args());
        if let Some(&slot) = self.cx.adt(d).and_then(|a| a.vslots.get(name)) {
            return Some(Resolved::Virtual {
                def: found.def(),
                slot,
                slots,
            });
        }
        let recv_ty = found.recv_ty(self.cx);
        Some(Resolved::Def {
            def: found.def(),
            slots,
            recv_ty,
            is_static: found.is_static(),
        })
    }

    /// Methods of interface values and bounded generic params.
    fn iface_method(&mut self, recv: TyId, name: &str) -> Option<Resolved> {
        let (bounds, on_param) = match self.cx.ty.kind(recv) {
            TyKind::Dyn(d, args) => (
                vec![Bound {
                    iface: *d,
                    args: args.clone(),
                }],
                false,
            ),
            TyKind::Param(n) => (
                self.bounds.get(*n as usize).cloned().unwrap_or_default(),
                true,
            ),
            _ => return None,
        };
        for b in bounds {
            let Some(i) = self.cx.iface(b.iface) else {
                continue;
            };
            if let Some(slot) = i.methods.iter().position(|m| m.name == name) {
                return Some(Resolved::Iface {
                    on_param,
                    iface: b.iface,
                    iface_args: b.args.clone(),
                    slot: slot as u32,
                    method: i.methods[slot].clone(),
                });
            }
        }
        None
    }

    /// Methods of `extend` blocks whose target type matches `recv`: an exact (non-generic)
    /// target wins over generic ones (`extend Array<i64>` over `extend<T> Array<T>`), and a
    /// generic block only applies when its bounds hold.
    fn extension_method(&mut self, recv: TyId, name: &str) -> Option<Resolved> {
        let n_ext = self.cx.extensions.len();
        let (exact, generic): (Vec<usize>, Vec<usize>) =
            (0..n_ext).partition(|&i| self.cx.extensions[i].generics.len() == 0);
        for i in exact.into_iter().chain(generic) {
            let Some(m) = self.cx.extensions[i].methods.get(name).copied() else {
                continue;
            };
            let (target, n) = (
                self.cx.extensions[i].target,
                self.cx.extensions[i].generics.len(),
            );
            let mut slots = vec![None; n];
            if !self.cx.match_ty(target, recv, &mut slots)
                || self.cx.ty.subst_known(target, &slots) != recv
            {
                continue;
            }
            let owner: Vec<TyId> = slots
                .iter()
                .map(|s| s.unwrap_or(self.cx.ty.error))
                .collect();
            if !self.extension_bounds_hold(i, &owner) {
                continue;
            }
            let slots = self.own_generic_slots(m.def, &owner);
            return Some(Resolved::Def {
                def: m.def,
                slots,
                recv_ty: recv,
                is_static: m.is_static,
            });
        }
        None
    }

    /// Do the type args `owner` of extension `i` satisfy its generic bounds?
    fn extension_bounds_hold(&mut self, i: usize, owner: &[TyId]) -> bool {
        let bounds = self.cx.extensions[i].generics.bounds.clone();
        bounds.iter().zip(owner).all(|(bs, &t)| {
            bs.iter().all(|b| {
                let b = Bound {
                    iface: b.iface,
                    args: b.args.iter().map(|a| self.cx.ty.subst(*a, owner)).collect(),
                };
                self.cx.satisfies(t, &b, &self.bounds)
            })
        })
    }

    fn builtin_method(&self, recv: TyId, name: &str) -> Option<BuiltinMethod> {
        let is_array = self.cx.ty.array_elem(recv).is_some();
        let atomic = self.cx.atomic_int(recv).is_some();
        match name {
            "push" if is_array => Some(BuiltinMethod::Push),
            "pop" if is_array => Some(BuiltinMethod::Pop),
            "add" if atomic => Some(BuiltinMethod::SharedAdd),
            "get" if atomic => Some(BuiltinMethod::SharedGet),
            "set" if atomic => Some(BuiltinMethod::SharedSet),
            "with" if self.cx.mutex_value(recv).is_some() => Some(BuiltinMethod::MutexWith),
            "clone" => Some(BuiltinMethod::Clone),
            "toString" if self.cx.ty.is_numeric(recv) || recv == self.cx.ty.bool_ => {
                Some(BuiltinMethod::ToString)
            }
            _ => None,
        }
    }

    /// Pass mode of a method's `this`.
    pub(crate) fn this_mode(&self, def: DefId) -> PassMode {
        self.cx
            .fn_info(def)
            .this
            .as_ref()
            .map_or(PassMode::Borrow, |t| t.mode)
    }
}
