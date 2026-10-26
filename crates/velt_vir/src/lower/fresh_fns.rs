//! Closures each evaluation of which is a new function value where JavaScript has one function
//! (`hir::Program::fresh_closures`, #943): a generic function value converted to another
//! function type, a function of fixed types asserted to a generic one, and a generic function's
//! value at a type over the caller's type parameters or at more than one type. Comparing such a
//! value would give a different answer than JavaScript, so a program that makes one and compares
//! function values of its type (`===`, `includes`, `indexOf`, a `Set` or `Map` key, through
//! generic code too) is reported, as are `unknown` values compared while one is held as one.
//!
//! The types are told apart by what survives instantiation: a function type of fixed types by
//! its number of parameters, a generic function type by its interface.

use velt_common::Diagnostic;
use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::Cx;

/// What [`Cx::fn_key`] tells function types apart by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum FnKey {
    Arity(usize),
    Generic(DefId),
}

impl Cx<'_> {
    /// The key of a function type, or of a generic function value's interface type.
    pub(super) fn fn_key(&self, ty: TyId) -> Option<FnKey> {
        match self.kind(ty) {
            TyKind::FnPtr { params, .. } => Some(FnKey::Arity(params.len())),
            // A generic closure's values are those of the generic function type it implements.
            TyKind::Closure(d) => match self.hir.impls.iter().find(|i| i.ty == ty) {
                Some(i) => Some(FnKey::Generic(i.iface)),
                None => Some(FnKey::Arity(self.fn_def(d).params.len())),
            },
            TyKind::Dyn(d, _) if self.is_generic_fn_value(ty) => Some(FnKey::Generic(d)),
            _ => None,
        }
    }

    /// Record that function values of type `ty` are compared (or hashed).
    pub(super) fn note_fn_compared(&mut self, ty: TyId) {
        self.note_fn_identity();
        if let Some(k) = self.fn_key(ty) {
            self.facts.fn_compared_keys.insert(k);
        }
    }

    /// Record that closure `def` is made, if it is a fresh one.
    pub(super) fn note_closure_made(&mut self, def: DefId) {
        if self.hir.fresh_closures.contains_key(&def) {
            self.facts.fresh_made.insert(def);
        }
    }

    /// Errors for the fresh closures made whose values may be compared (module docs).
    pub(super) fn fresh_fn_diagnostics(&self) -> Vec<Diagnostic> {
        let mut made: Vec<DefId> = self.facts.fresh_made.iter().copied().collect();
        made.sort_unstable_by_key(|d| d.0);
        let mut out = vec![];
        let mut spans = rustc_hash::FxHashSet::default();
        for d in made {
            let Some(key) = self
                .hir
                .fresh_closures
                .get(&d)
                .and_then(|t| self.fn_key(*t))
            else {
                continue;
            };
            let typed = self.facts.fn_compared_keys.contains(&key);
            let dynamic = self.facts.any_identity
                && (self
                    .facts
                    .dyn_types
                    .iter()
                    .any(|t| self.fn_key(*t) == Some(key))
                    || self.facts.generic_any_keys.contains(&key));
            if !typed && !dynamic {
                continue;
            }
            let span = match self.hir.def(d) {
                hir::Def::Fn(f) => f.span,
                _ => continue,
            };
            // A function's values at several types are reported once, at the function.
            if !spans.insert(span) {
                continue;
            }
            let how = match typed {
                true => "the program compares function values of its type (`===`, `includes`, `indexOf`, a `Set` or `Map` key)",
                false => "the program compares `unknown` or `any` values, which may hold it",
            };
            out.push(
                Diagnostic::error(
                    "this function value can't be compared yet: each evaluation makes a new function, where JavaScript keeps one (#943)",
                    span,
                )
                .with_note(how)
                .with_note("a generic function converted to another function type, a function asserted to a generic type, and a generic function used as a value at more than one type, or at a type over type parameters, are wrapped at each use; compare the functions themselves, or keep one value in a variable"),
            );
        }
        out
    }
}
