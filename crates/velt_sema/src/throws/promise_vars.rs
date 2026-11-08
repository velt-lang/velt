//! Error-set variables of promise types written without an error type (`promise_sites`,
//! `throws::vars`).
//!
//! In a check with variables, a `Promise<T>` site written as the result of a function type, a
//! function or a closure is interned once as `Promise<T, V>` with a variable `V` of its own,
//! and a function type whose result it is shares it (`fn_types`): what the function type's
//! values throw is what their promises reject with. Then, while bodies are checked:
//! - an async closure created for the site's type rejects into `V` (`throws::checks`, as any
//!   closure created for a site's type throws into its variable);
//! - a value of another site's promise type stored as this one's: the two are one set
//!   (`Ctx::unify_err_vars`);
//! - a `new Promise` or `Promise.withResolvers` expected as a `Promise<T, V>` rejects into `V`;
//! - a promise of another rejection type `E` stored as a value of `Promise<T, V>`, and a value
//!   of type `E` passed to such a `reject`: it is retyped, and `V`'s set must be
//!   exactly `E` (`Ctx::require_set`), since a check without variables would widen a promise
//!   of a smaller set;
//! - a function value stored where its type is written: what its promises reject with that
//!   its type couldn't say goes into `V` (`Ctx::bind_stored_fn`).
//!
//! Awaiting a value of type `Promise<T, V>` throws `V`. Whatever else stores into a site
//! (another conversion, a `reject(null)`) seeds the site and checks the program again with the
//! solved sets, as before (`Ctx::seed_site_with`). The other sites (a type argument, as in
//! `implements Getter<Promise<T>>`, a method's result, a variable's or a field's type) have no
//! variable: their promise types are compared as written (bounds, implementations, the results
//! of methods and of the ones they implement), so they take their errors by checking again.

use velt_common::Span;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

impl Ctx<'_> {
    /// `t` resolved, a `Promise<T>` written there without an error type getting a variable in a
    /// check with them (the result of a function type, a function or a closure).
    pub(crate) fn resolve_promise_var_at(
        &mut self,
        t: &ast::TypeExpr,
        env: &crate::resolve::TyEnv,
    ) -> TyId {
        let outer = self.fn_sites.promise_var_at.replace(t.span);
        let r = self.resolve_type(t, env);
        self.fn_sites.promise_var_at = outer;
        r
    }

    /// The variable of the `Promise<T>` site at `site`, in a check with variables.
    fn promise_site_var(&self, site: Span) -> Option<TyId> {
        if !self.fn_sites.user_vars {
            return None;
        }
        self.fn_sites.var_of.get(&site).copied()
    }

    /// What a function value stored into site `site` rejects with, `e`, beyond what its type
    /// says: added to the site's variable (`e` may hold other variables, which it then
    /// contains). False when the site has none or `e` can't be kept.
    pub(super) fn reject_into_site_var(&mut self, site: Span, e: TyId) -> bool {
        let Some(v) = self.promise_site_var(site) else {
            return false;
        };
        if !self.throw_into_vars(Some(v), Some(e)) {
            return false;
        }
        self.note_site_seed(site, e);
        true
    }

    /// A value of type `from` stored as a value of `to`, where `to` is `Promise<T, V>` (a
    /// site's type with its variable) and `from` a `Promise<T, E>`, or `to` is the variable `V`
    /// a `reject` takes (`FnCx::open_reject`) and `from` an error type `E`: `E` goes into `V`
    /// (the variables in it into its set), whose set must be exactly `E`. True when the value
    /// is to be retyped as `to`.
    pub(crate) fn store_into_promise_var(&mut self, from: TyId, to: TyId) -> bool {
        if !self.fn_sites.user_vars || self.fn_sites.vars.is_empty() {
            return false;
        }
        let reject = self.fn_sites.rejects.iter().rev().find(|(r, _)| *r == to);
        let (e, f, site) = match (self.ty.kind(from), self.ty.kind(to), reject) {
            (&TyKind::Promise(x, e), &TyKind::Promise(y, f), _) if x == y => {
                (e, f, self.target_site(to))
            }
            (_, _, Some(&(_, at))) => (from, to, Some(at)),
            _ => return false,
        };
        let Some(v) = self.err_var(f) else {
            return false;
        };
        if e == f || self.ty.has_error(e) {
            return false;
        }
        // Only a type another check can hold (`fn_types::seed_fn_sites`).
        let canon = self.canon_error(Some(e));
        if !self.throw_into_vars(Some(f), canon) {
            return false;
        }
        self.require_set(v, canon.unwrap_or(self.ty.never));
        if let (Some(site), Some(e)) = (site, canon) {
            self.note_site_seed(site, e);
        }
        true
    }

    /// What a check without variables would add to site `site` for a store the variables took
    /// (`e`, its members without variables): kept apart from what this check found (which
    /// calls through function types read), so a check without variables after this one
    /// (`FnSites::fallback`, `FnSites::doubted`) starts from the sets a check without them
    /// would have found.
    fn note_site_seed(&mut self, site: Span, e: TyId) {
        for m in self.error_members(e) {
            if self.is_err_var(m) {
                continue;
            }
            let Some(p) = self.port_thrown(m) else {
                continue;
            };
            let set = self.fn_sites.quiet.entry(site).or_default();
            if !set.contains(&p) {
                set.push(p);
            }
        }
    }

    /// Variable `v`'s set must be exactly error type `t` (a value of type `t` was retyped as
    /// one of `v`).
    fn require_set(&mut self, v: u32, t: TyId) {
        self.fn_sites.var_sets.ensure(v);
        self.fn_sites.var_sets.exact.push((v, t));
    }
}
