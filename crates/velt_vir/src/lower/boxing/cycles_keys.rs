//! The function types closure environments are told apart by in the cycle analysis
//! (docs/internals/design/cycles.md section 1, step 2): a function value of type `F` can only
//! hold the environment of a closure made at a type with `F`'s key, so the analysis follows a
//! function value to those environments only, not to every closure of the program.
//!
//! Sema never uses a function value as one of another type as it is (velt_sema `fn_widen.rs`:
//! its parameters would read arguments laid out differently); a conversion that changes what a
//! function takes or returns, or widens what it throws, makes a new closure (a wrapper, which
//! captures the value it wraps, `hir::Program::fn_wrappers`). Sema retypes a value without a
//! wrapper only where the key below is the same on both sides:
//! - types that differ only in error-set variables (`coerce.rs` `unify_err_vars`,
//!   `coerce_vars.rs`): every error-set site (`TyKind::Op`) is one marker in the key;
//! - a function returning `never` used as one returning `void` (`never_result_converts`): a
//!   `never` result is `void` in the key;
//! - one generic function type at other type arguments (`same_generic_fn`, an `Upcast`): a
//!   generic function value's key is its interface alone;
//! - readonly twins, `unknown` for `any`, brands, two forms of one object type: one type once
//!   the program is erased and lowering substitutes and canonicalizes (`sub`).
//!
//! A closure whose own type is not a function type of fixed types (a generic closure, held as
//! its interface) has no key; every function value may lead to those.

use velt_sema::hir::{DefId, TyId, TyKind};

use crate::lower::{Cx, FnLower};

/// What a function value's environment is told apart by.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::lower) enum CycleKey {
    /// A function type of fixed types: parameters, result (`void` for `never`), what it
    /// throws, and whether the last parameter is a rest parameter.
    Fixed(Vec<TyId>, TyId, TyId, bool),
    /// A generic function value, by its interface.
    Generic(DefId),
}

impl CycleKey {
    /// The types the key names.
    pub(in crate::lower) fn tys(&self) -> Vec<TyId> {
        match self {
            CycleKey::Fixed(ps, r, t, _) => ps.iter().copied().chain([*r, *t]).collect(),
            CycleKey::Generic(_) => vec![],
        }
    }

    /// The key with every type replaced through `map`.
    pub(in crate::lower) fn map(&self, map: &mut dyn FnMut(TyId) -> TyId) -> CycleKey {
        match self {
            CycleKey::Fixed(ps, r, t, b) => {
                CycleKey::Fixed(ps.iter().map(|&p| map(p)).collect(), map(*r), map(*t), *b)
            }
            CycleKey::Generic(d) => CycleKey::Generic(*d),
        }
    }
}

impl Cx<'_> {
    /// The key of the function type `t` (instantiated); `None` when `t` is not the type of a
    /// function value of fixed types or of a generic function value.
    pub(in crate::lower) fn cycle_key(&mut self, t: TyId) -> Option<CycleKey> {
        match self.kind(t) {
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => {
                let params = params.iter().map(|&p| self.key_ty(p, 0)).collect();
                let ret = match self.kind(ret) {
                    TyKind::Never => self.intern(TyKind::Unit),
                    _ => self.key_ty(ret, 0),
                };
                let throws = self.key_ty(throws, 0);
                Some(CycleKey::Fixed(params, ret, throws, rest))
            }
            TyKind::Dyn(d, _) if self.is_generic_fn_value(t) => Some(CycleKey::Generic(d)),
            _ => None,
        }
    }

    /// `t` with every error-set site (`TyKind::Op`) replaced by one marker, `TyKind::Error`
    /// (no value of a lowered program has that type), wherever it appears.
    fn key_ty(&mut self, t: TyId, depth: u32) -> TyId {
        if depth > 16 {
            return t;
        }
        let w = |cx: &mut Self, x: TyId| cx.key_ty(x, depth + 1);
        let k = match self.kind(t) {
            TyKind::Op(..) => TyKind::Error,
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => TyKind::FnPtr {
                params: params.iter().map(|&p| w(self, p)).collect(),
                ret: w(self, ret),
                throws: w(self, throws),
                rest,
            },
            TyKind::Promise(v, e) => TyKind::Promise(w(self, v), w(self, e)),
            TyKind::Adt(d, args) => TyKind::Adt(d, args.iter().map(|&a| w(self, a)).collect()),
            TyKind::Dyn(d, args) => TyKind::Dyn(d, args.iter().map(|&a| w(self, a)).collect()),
            TyKind::Array(e) => TyKind::Array(w(self, e)),
            TyKind::Map(k, v) => TyKind::Map(w(self, k), w(self, v)),
            TyKind::Tuple(es) => TyKind::Tuple(es.iter().map(|&a| w(self, a)).collect()),
            TyKind::Option(e, n) => TyKind::Option(w(self, e), n),
            TyKind::Result(a, b) => TyKind::Result(w(self, a), w(self, b)),
            TyKind::Shared(e) => TyKind::Shared(w(self, e)),
            _ => return t,
        };
        self.intern(k)
    }

    /// Record that a table of interface `iface` was built for the concrete type `t`.
    pub(in crate::lower) fn note_iface_impl(&mut self, iface: DefId, t: TyId) {
        self.facts.iface_impls.insert((iface, t));
    }
}

impl FnLower<'_, '_> {
    /// Record that closure `def` is made here, its value of type `ty` (before substitution).
    pub(in crate::lower) fn note_closure_env(&mut self, def: DefId, ty: TyId) {
        let t = self.sub(ty);
        let key = self.cx.cycle_key(t);
        let targs = self.targs.clone();
        self.cx.facts.closure_envs.insert((key, def, targs));
    }
}
