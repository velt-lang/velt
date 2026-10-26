//! Closures each evaluation of which is a new function value where JavaScript has one function
//! (`hir::Program::fresh_closures`, #943): a generic function value converted to another
//! function type, a function of fixed types asserted to a generic one, and a generic function's
//! value at a type over the caller's type parameters or at more than one type. Comparing such a
//! value would give a different answer than JavaScript, so a program that makes one and compares
//! function values of its type (`===`, `includes`, `indexOf`, a `Set` or `Map` key, through
//! generic code too) is reported, as are `unknown` values compared while one is held as one.
//!
//! The types are told apart as instantiated, without what they throw: a function type of fixed
//! types by its parameter and result types, a generic function type by its interface and that
//! interface's arguments. A comparison counts where it is lowered in a function (`===`, a `Set`
//! instance at the type), not in the glue built for a type (an `any` table's equality entry is
//! there whether or not anything is compared: comparing `any` values is `any_identity`).

use velt_common::Diagnostic;
use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::{Cx, FnLower};

/// What [`Cx::fn_key`] tells function types apart by.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum FnKey {
    Fixed(Vec<TyId>, TyId, bool),
    Generic(DefId, Vec<TyId>),
}

impl Cx<'_> {
    /// The key of a function type, or of a generic function value's interface type (`None` for
    /// a generic closure's own type: its values are those of its interface type).
    pub(super) fn fn_key(&self, ty: TyId) -> Option<FnKey> {
        match self.kind(ty) {
            TyKind::FnPtr {
                params, ret, rest, ..
            } => Some(FnKey::Fixed(params, ret, rest)),
            TyKind::Closure(_) if self.hir.impls.iter().any(|i| i.ty == ty) => None,
            TyKind::Closure(d) => {
                let f = self.fn_def(d);
                let params = f.params.iter().map(|p| p.ty).collect();
                Some(FnKey::Fixed(params, f.ret, false))
            }
            TyKind::Dyn(d, args) if self.is_generic_fn_value(ty) => Some(FnKey::Generic(d, args)),
            _ => None,
        }
    }

    /// Errors for the fresh closures made whose values may be compared (module docs).
    pub(super) fn fresh_fn_diagnostics(&self) -> Vec<Diagnostic> {
        let mut made: Vec<(DefId, FnKey)> = self.facts.fresh_made.iter().cloned().collect();
        made.sort_unstable_by_key(|(d, _)| d.0);
        let mut out = vec![];
        let mut spans = rustc_hash::FxHashSet::default();
        for (d, key) in made {
            let typed = self.facts.fn_compared_keys.contains(&key);
            let dynamic = self.facts.any_identity
                && (self
                    .facts
                    .dyn_types
                    .iter()
                    .any(|t| self.fn_key(*t).as_ref() == Some(&key))
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
                false => "the program compares `unknown` or `any` values while one holds it",
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

impl FnLower<'_, '_> {
    /// Record that function values of type `ty` are compared (or hashed) here.
    pub(super) fn note_fn_compared(&mut self, ty: TyId) {
        self.cx.note_fn_identity();
        if self.glue {
            return;
        }
        if let Some(k) = self.cx.fn_key(ty) {
            self.cx.facts.fn_compared_keys.insert(k);
        }
    }

    /// Record that closure `def` is made here, at its instantiated type, if it is a fresh one.
    pub(super) fn note_closure_made(&mut self, def: DefId) {
        let Some(&t) = self.cx.hir.fresh_closures.get(&def) else {
            return;
        };
        // Instantiated without what it throws (an error-set site sema reduces only per site).
        let key = match self.cx.kind(t) {
            TyKind::FnPtr {
                params, ret, rest, ..
            } => FnKey::Fixed(
                params.iter().map(|p| self.sub(*p)).collect(),
                self.sub(ret),
                rest,
            ),
            TyKind::Dyn(d, args) => FnKey::Generic(d, args.iter().map(|a| self.sub(*a)).collect()),
            _ => return,
        };
        self.cx.facts.fresh_made.insert((def, key));
    }
}
