//! Inferred rejection types of promise types written without one (docs/reference/async.md
//! "Errors").
//!
//! TypeScript's `Promise<T>` may reject with any value, so a `Promise<T>` written without an
//! error type in a user module is a site, as a function type written without `throws` is
//! (`fn_types`): a promise that rejects with `E`, stored where the site's type is expected (a
//! returned value, a typed variable or field, an argument, an element pushed to a typed array or
//! set as a typed `Map`'s value), adds `E` to that site's set, and the program is checked again
//! with `Promise<T, E>` written there, so awaiting a value of that type throws `E`. Only a store
//! into a known site counts: matched by type, the errors would reach every `Promise<T>` of the
//! program. The result type of an async function is no site: its errors are what its body
//! throws. A `new Promise(…)` or `Promise.withResolvers<T>()` without a written error type
//! rejects with what its `reject` is called with, once its promise is stored into a site.
//! A program that stores no rejecting promise into such a type is checked once, with every site
//! `never`.

use velt_common::Span;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

/// The type written in a declaration's initializer that the declared value has (`new
/// Map<string, Promise<T>>()`, `[] as Promise<T>[]`): where its sites are.
pub(crate) fn written_init_type(e: &ast::Expr) -> Option<&ast::TypeExpr> {
    match &e.kind {
        ast::ExprKind::New { class, .. } => Some(class),
        ast::ExprKind::Cast { ty, .. } => Some(ty),
        ast::ExprKind::Paren(inner) => written_init_type(inner),
        _ => None,
    }
}

impl Ctx<'_> {
    /// The rejection type of `Promise<payload>` written at `span` in module `module` without an
    /// error type: what is stored into it (`never` until something is), recording the site.
    pub(crate) fn promise_site(&mut self, payload: TyId, span: Span, module: usize) -> TyId {
        let never = self.ty.never;
        let user = self.scopes.get(module).is_some_and(|s| !s.is_std);
        if !user || span == Span::DUMMY || self.fn_sites.async_ret {
            return self.ty.intern(TyKind::Promise(payload, never));
        }
        let e = self.site_error(span, module).unwrap_or(never);
        let t = self.ty.intern(TyKind::Promise(payload, e));
        self.record_fn_site(span, module, t);
        t
    }

    /// The written result type `t` of a function, `is_async`: then its `Promise<T>` is no site.
    pub(crate) fn resolve_async_aware(
        &mut self,
        t: &ast::TypeExpr,
        env: &crate::resolve::TyEnv,
        is_async: bool,
    ) -> TyId {
        let outer = std::mem::replace(&mut self.fn_sites.async_ret, is_async);
        let r = self.resolve_type(t, env);
        self.fn_sites.async_ret = outer;
        r
    }

    /// A promise of type `from` is stored as a value of promise type `to`, which does not allow
    /// what it rejects with: when `to` is the site the value is stored into, add those errors to
    /// it, so the program is checked again with them. False when the store is reported instead
    /// (no site, the last check, or an error type that can't be kept).
    pub(crate) fn seed_promise_site(&mut self, from: TyId, to: TyId) -> bool {
        let (&TyKind::Promise(_, e), &TyKind::Promise(..)) = (self.ty.kind(from), self.ty.kind(to))
        else {
            return false;
        };
        match self.target_site(to) {
            Some(site) => self.seed_site_with(site, e),
            None => false,
        }
    }

    /// Add what `e` holds to site `site` (the program is checked again with it); false when the
    /// check is the last one or `e` can't be kept.
    pub(crate) fn seed_site_with(&mut self, site: Span, e: TyId) -> bool {
        if self.fn_sites.is_last() {
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
            match self.port_thrown(m) {
                Some(p) => members.push(p),
                None => return false,
            }
        }
        self.add_seed(site, members);
        // A check with error-set variables writes their solved sets into the seeds of the next
        // one only when it can't use them (`throws::vars`): this growth needs the next check.
        self.fn_sites.var_sets.unsupported = true;
        true
    }

    /// A value of type `found` is converted to `exp`, which is the open error type of a
    /// `new Promise` executor being checked or of a `Promise.withResolvers` (its `reject`'s
    /// parameter): add `found` to the site of that promise.
    pub(crate) fn seed_reject(&mut self, found: TyId, exp: TyId) -> bool {
        let sites: Vec<Span> = (self.fn_sites.rejects.iter())
            .filter(|(e, _)| *e == exp)
            .map(|(_, s)| *s)
            .collect();
        let mut seeded = false;
        for s in sites {
            seeded |= self.seed_site_with(s, found);
        }
        seeded
    }

    /// The result `have` of a method (its site `have_site`) and the result `want` of the
    /// interface or base method it implements or overrides (site `want_site`): promise types of
    /// one payload written without error types reject with the union of both, so each site
    /// takes the other's errors. True when the program is checked again with them (the
    /// mismatch is then not reported).
    pub(crate) fn unify_promise_results(
        &mut self,
        (have, have_site): (TyId, Option<Span>),
        (want, want_site): (TyId, Option<Span>),
    ) -> bool {
        let (&TyKind::Promise(a, e), &TyKind::Promise(b, f)) =
            (self.ty.kind(have), self.ty.kind(want))
        else {
            return false;
        };
        let (Some(hs), Some(ws)) = (have_site, want_site) else {
            return false;
        };
        if a != b || e == f || self.fn_sites.is_last() {
            return false;
        }
        // The sets as found so far (classes are not portable yet while signatures are collected).
        self.site_flow(hs, ws);
        self.site_flow(ws, hs);
        self.fn_sites.var_sets.unsupported = true;
        self.fn_sites.grew
    }

    /// `t` without its nothings, or the promise type of an array's elements or a `Map`'s values:
    /// the type of the site a value of type `t` is stored into or read from.
    pub(super) fn site_core(&self, t: TyId) -> TyId {
        let t = self.ty.core(t).unwrap_or(t);
        let inner = match self.ty.kind(t) {
            TyKind::Array(e) => *e,
            TyKind::Adt(d, args) if args.len() == 2 && Some(*d) == self.prelude_adt("Map") => {
                args[1]
            }
            _ => return t,
        };
        let inner = self.ty.core(inner).unwrap_or(inner);
        match self.ty.kind(inner) {
            TyKind::Promise(..) => inner,
            _ => t,
        }
    }
}

impl crate::body::FnCx<'_, '_> {
    /// The value argument of `xs.push(v)`, `xs.unshift(v)` or `m.set(k, v)` on an array or
    /// `Map` of promises whose type is a site: that argument's span and the site.
    pub(crate) fn stored_arg_site(
        &self,
        recv: &crate::hir::Expr,
        name: &str,
        args: &[ast::Expr],
    ) -> Option<(Span, Span)> {
        let arg = match (name, args) {
            ("push" | "unshift", [v]) | ("set", [_, v]) => v,
            _ => return None,
        };
        let site = self.site_of_value(recv)?;
        Some((arg.span, site))
    }

    /// The error type of `new Promise<T>(…)` or `Promise.withResolvers<T>()` at `span` without
    /// a written one (`e`, from the expected type or `never`): it is a site of its own, which
    /// takes what `reject` is called with ([`Ctx::seed_reject`]). Returns the error type and
    /// whether `reject`'s parameter was registered (the caller pops it after an executor).
    pub(crate) fn open_reject(&mut self, t: TyId, e: TyId, span: Span) -> (TyId, bool) {
        let own = self.cx.promise_site(t, span, self.module);
        let Some(own_e) = self.cx.ty.promise_error(own) else {
            return (e, false);
        };
        if !self.cx.fn_sites.by_file.contains_key(&span.file) {
            return (e, false);
        }
        let e = self.cx.join_errors(Some(e), Some(own_e)).unwrap_or(e);
        self.cx.fn_sites.rejects.push((e, span));
        (e, true)
    }
}

impl crate::body::FnCx<'_, '_> {
    /// `{ list: [] as Promise<T>[] }`: the type of a field of anonymous object type `def` is
    /// written in its value, where its sites are.
    pub(crate) fn note_anon_written<'e>(
        &mut self,
        def: crate::hir::DefId,
        values: impl Iterator<Item = Option<&'e ast::Expr>>,
    ) {
        for (i, value) in values.enumerate() {
            if let Some(t) = value.and_then(written_init_type) {
                let at = (def, i as u32);
                self.cx.fn_sites.anon_written.entry(at).or_insert(t.span);
            }
        }
    }
}
