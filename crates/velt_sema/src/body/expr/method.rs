//! Method resolution on a receiver type. Lookup order: the type's own methods (classes: then
//! base classes), default methods of interfaces it implements, interface methods (receivers of
//! interface type or bounded generic type), `extend` blocks whose target matches, then the
//! builtin methods (`push`/`pop` on arrays, `clone` everywhere).
//!
//! Dispatch: a class method with a vtable slot in the receiver's static class is
//! `Callee::Virtual`; everything else on concrete types is a direct `Callee::Def`.

use velt_common::{Diagnostic, Span};

use crate::body::FnCx;
use crate::collect::lookup_method;
use crate::defs::{Bound, IfaceMethod};
use crate::hir::{DefId, PassMode, TyId, TyKind};
use crate::types::children;

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
    /// `with(f)` on `Mutex<T>` / `shared<Mutex<T>>`, `write(f)` on an `RwLock`.
    MutexWith,
    /// `read(f)` on `RwLock<T>` / `shared<RwLock<T>>`.
    LockRead,
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
            // A subclass's static of the name doesn't hide an inherited `extend` instance
            // method, as it doesn't hide an inherited instance method.
            if matches!(
                r,
                Resolved::Def {
                    is_static: true,
                    ..
                }
            ) {
                if let Some(e) = self.inherited_extension_method(recv, name) {
                    if !matches!(
                        e,
                        Resolved::Def {
                            is_static: true,
                            ..
                        }
                    ) {
                        return Some(e);
                    }
                }
            }
            return Some(r);
        }
        if let Some(r) = self.iface_method(recv, name) {
            return Some(r);
        }
        if let Some(r) = self.extension_method(recv, name) {
            return Some(r);
        }
        if let Some(r) = self.inherited_extension_method(recv, name) {
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
        // `o.#m()` in the body of class `C` is `C`'s `#m`, also on a subclass instance that
        // declares a `#m` of its own (never virtual).
        let recv = match (self.name_owner(name), crate::defs::is_private_key(name)) {
            (Some(owner), true) => self.ancestor(recv, owner),
            _ => recv,
        };
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
            TyKind::Param(n) => {
                // Declared bounds first, then those of blanket `extend … implements` blocks.
                let mut bs = self.bounds.get(*n as usize).cloned().unwrap_or_default();
                if !self.cx.ext_impls.is_empty() {
                    let scope = self.bounds.clone();
                    bs.extend(self.cx.blanket_bounds(recv, &scope));
                }
                (bs, true)
            }
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

    /// The method `name` of the `extend` block for `recv`: among the applicable blocks (target
    /// matches, bounds hold) the most specific one ([`FnCx::most_specific`]; `extend Array<i64>`
    /// over `extend<T> Array<T[]>` over `extend<T> Array<T>`). Several equally specific blocks are ambiguous
    /// ([`FnCx::check_extension_ambiguity`] reports it at the call); the first is used then.
    fn extension_method(&mut self, recv: TyId, name: &str) -> Option<Resolved> {
        let candidates = self.applicable_extensions(recv, name);
        let (i, owner) = self
            .most_specific(&candidates)
            .first()
            .map(|&k| candidates[k].clone())?;
        let m = self.cx.extensions[i].methods.get(name).copied()?;
        let slots = self.own_generic_slots(m.def, &owner);
        Some(Resolved::Def {
            def: m.def,
            slots,
            recv_ty: recv,
            is_static: m.is_static,
        })
    }

    /// The method `name` of an `extend` block of a base class of class `recv`, nearest base
    /// first: a subclass has its bases' extension methods as it has their own methods
    /// (`class MyRes extends Response {}` has `Response`'s `bytes()`, std/fetch/typed.vlt).
    fn inherited_extension_method(&mut self, recv: TyId, name: &str) -> Option<Resolved> {
        let base = self.cx.base_of(recv)?;
        self.base_extension_method(base, name)
    }

    /// The method `name` of an `extend` block of class `t` or of one of its base classes,
    /// nearest first (`super.m()` too).
    pub(super) fn base_extension_method(&mut self, t: TyId, name: &str) -> Option<Resolved> {
        let mut t = t;
        for _ in 0..64 {
            if let Some(r) = self.extension_method(t, name) {
                return Some(r);
            }
            t = self.cx.base_of(t)?;
        }
        None
    }

    /// An error if `r` (a resolved method `name`) comes from an `extend` block and several
    /// equally specific blocks give the type it was found on a method `name`.
    pub(crate) fn check_extension_ambiguity(&mut self, r: &Resolved, name: &str, span: Span) {
        let Resolved::Def { def, recv_ty, .. } = r else {
            return;
        };
        // The type the method was found on (a base class's, for an inherited `extend` method).
        let recv = *recv_ty;
        let from_extension = self
            .cx
            .extensions
            .iter()
            .any(|x| x.methods.get(name).is_some_and(|m| m.def == *def));
        if !from_extension {
            return;
        }
        let candidates = self.applicable_extensions(recv, name);
        let best = self.most_specific(&candidates);
        if best.len() < 2 {
            return;
        }
        let tn = self.cx.display(recv);
        let mut d = Diagnostic::error(
            format!("ambiguous extension method `{name}` on `{tn}`"),
            span,
        );
        for k in best {
            let m = self.cx.extensions[candidates[k].0].methods[name];
            d = d.with_label(self.cx.fn_info(m.def).name_span, "one candidate");
        }
        self.cx.error(d.with_note(
            "neither `extend` block's target is more specific than the other's: rename one method",
        ));
    }

    /// `extend` blocks with a method `name` that apply to `recv`, with their type arguments.
    fn applicable_extensions(&mut self, recv: TyId, name: &str) -> Vec<(usize, Vec<TyId>)> {
        let mut out = vec![];
        let n = self.cx.extensions.len();
        if self.cx.extension_names.0 != n {
            let mut names: rustc_hash::FxHashMap<String, Vec<usize>> = Default::default();
            for (i, x) in self.cx.extensions.iter().enumerate() {
                for k in x.methods.keys() {
                    names.entry(k.clone()).or_default().push(i);
                }
            }
            self.cx.extension_names = (n, names);
        }
        let Some(found) = self.cx.extension_names.1.get(name) else {
            return out;
        };
        for i in found.clone() {
            let target = self.cx.extensions[i].target;
            let Some(slots) = self.instance_args(target, recv) else {
                continue;
            };
            let owner: Vec<TyId> = slots
                .iter()
                .map(|s| s.unwrap_or(self.cx.ty.error))
                .collect();
            if self.extension_bounds_hold(i, &owner) {
                out.push((i, owner));
            }
        }
        out
    }

    /// The type arguments that make `pattern` (an `extend` target) equal to `t`, if any.
    fn instance_args(&mut self, pattern: TyId, t: TyId) -> Option<Vec<Option<TyId>>> {
        let n = self.max_param(pattern).map_or(0, |p| p + 1);
        let mut slots = vec![None; n];
        let ok =
            self.cx.match_ty(pattern, t, &mut slots) && self.cx.subst_known(pattern, &slots) == t;
        ok.then_some(slots)
    }

    /// The candidates (indexes into `candidates`) that no other candidate is strictly more
    /// specific than, in declaration order. A is more specific than B when A's target is an
    /// instance of B's and not the other way round, or when both targets are the same pattern
    /// and only A has bounds (`extend<T extends Comparable<T>> Array<T>` over
    /// `extend<T> Array<T>`).
    fn most_specific(&mut self, candidates: &[(usize, Vec<TyId>)]) -> Vec<usize> {
        let blocks: Vec<(TyId, bool)> = candidates
            .iter()
            .map(|(i, _)| {
                let x = &self.cx.extensions[*i];
                (x.target, x.generics.bounds.iter().any(|b| !b.is_empty()))
            })
            .collect();
        (0..blocks.len())
            .filter(|&a| {
                !(0..blocks.len()).any(|b| b != a && self.more_specific(blocks[b], blocks[a]))
            })
            .collect()
    }

    /// Is block `a` (target, has bounds) strictly more specific than block `b`?
    fn more_specific(&mut self, a: (TyId, bool), b: (TyId, bool)) -> bool {
        if self.instance_args(b.0, a.0).is_none() {
            return false;
        }
        self.instance_args(a.0, b.0).is_none() || (a.1 && !b.1)
    }

    /// The highest `Param` index in `t`.
    fn max_param(&self, t: TyId) -> Option<usize> {
        let k = self.cx.ty.kind(t);
        if let TyKind::Param(p) = k {
            return Some(*p as usize);
        }
        children(k)
            .into_iter()
            .filter_map(|c| self.max_param(c))
            .max()
    }

    /// Do the type args `owner` of extension `i` satisfy its generic bounds?
    fn extension_bounds_hold(&mut self, i: usize, owner: &[TyId]) -> bool {
        let bounds = self.cx.extensions[i].generics.bounds.clone();
        bounds.iter().zip(owner).all(|(bs, &t)| {
            bs.iter().all(|b| {
                let b = Bound {
                    iface: b.iface,
                    args: b.args.iter().map(|a| self.cx.subst(*a, owner)).collect(),
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
            "with" if self.cx.mutex_value(recv).is_some() && !self.cx.is_rwlock(recv) => {
                Some(BuiltinMethod::MutexWith)
            }
            "write" if self.cx.is_rwlock(recv) => Some(BuiltinMethod::MutexWith),
            "read" if self.cx.is_rwlock(recv) => Some(BuiltinMethod::LockRead),
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
