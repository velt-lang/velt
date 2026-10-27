//! Inferred rejection types of promise types written without one (docs/reference/async.md
//! "Errors").
//!
//! TypeScript's `Promise<T>` may reject with any value, so a `Promise<T>` written without an
//! error type in a user module is a site, as a function type written without `throws` is
//! (`fn_types`): a promise that rejects with `E`, stored where the site's type is expected (a
//! returned value, a typed variable or field, an argument, an element pushed to a typed array),
//! adds `E` to that site's set, and the program is checked again with `Promise<T, E>` written
//! there, so awaiting a value of that type throws `E`. Only a store into a known site counts:
//! matched by type, the errors would reach every `Promise<T>` of the program. A program that
//! stores no rejecting promise into such a type is checked once, with every site `never`.

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

impl Ctx<'_> {
    /// The rejection type of `Promise<payload>` written at `span` in module `module` without an
    /// error type: what is stored into it (`never` until something is), recording the site.
    pub(crate) fn promise_site(&mut self, payload: TyId, span: Span, module: usize) -> TyId {
        let never = self.ty.never;
        let user = self.scopes.get(module).is_some_and(|s| !s.is_std);
        if !user || span == Span::DUMMY {
            return self.ty.intern(TyKind::Promise(payload, never));
        }
        let e = self.site_error(span, module).unwrap_or(never);
        let t = self.ty.intern(TyKind::Promise(payload, e));
        self.record_fn_site(span, module, t);
        t
    }

    /// A promise of type `from` is stored as a value of promise type `to` (both with one
    /// payload), which does not allow what it rejects with: when `to` is the site the value is
    /// stored into, add those errors to it, so the program is checked again with them. False
    /// when the store is reported instead (no site, the last check, or an error type that can't
    /// be kept).
    pub(crate) fn seed_promise_site(&mut self, from: TyId, to: TyId) -> bool {
        let (&TyKind::Promise(_, e), &TyKind::Promise(..)) = (self.ty.kind(from), self.ty.kind(to))
        else {
            return false;
        };
        if self.fn_sites.is_last() {
            return false;
        }
        // Where the site is not known (a `Map` value), the sites of the type: as for function
        // types, only `Error` classes then.
        let known = self.target_site(to);
        let sites: Vec<Span> = match known {
            Some(s) => vec![s],
            None => (self.fn_sites.sites.iter())
                .filter(|(_, g)| matches!(self.ty.kind(*g), TyKind::Promise(..)))
                .filter(|(_, g)| self.site_matches(*g, to, 0))
                .map(|(s, _)| *s)
                .collect(),
        };
        if sites.is_empty() {
            return false;
        }
        // An error-set variable (a promise from a call through a function value) is only solved
        // after the bodies: check again with the sets written in, where it is a set.
        if self.mentions_err_var(e) {
            self.fn_sites.var_sets.unsupported = true;
            self.fn_sites.fallback();
            return true;
        }
        let mut members = vec![];
        for m in self.error_members(e) {
            if known.is_none() && !self.is_error_class(m) {
                return false;
            }
            match self.port_type(m) {
                Some(p) => members.push(p),
                None => return false,
            }
        }
        for s in sites {
            self.add_seed(s, members.clone());
        }
        // A check with error-set variables writes their solved sets into the seeds of the next
        // one only when it can't use them (`throws::vars`): this growth needs the next check.
        self.fn_sites.var_sets.unsupported = true;
        true
    }

    /// `t` without its nothings, or the element type of an array of promises: the type of the
    /// site a value of type `t` is stored into or read from.
    pub(super) fn site_core(&self, t: TyId) -> TyId {
        let t = self.ty.core(t).unwrap_or(t);
        match self.ty.kind(t) {
            TyKind::Array(e) => {
                let e = self.ty.core(*e).unwrap_or(*e);
                match self.ty.kind(e) {
                    TyKind::Promise(..) => e,
                    _ => t,
                }
            }
            _ => t,
        }
    }
}
