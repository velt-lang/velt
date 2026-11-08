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
use crate::defs::DefInfo;
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
        if !self.store_into_var(v, Some(e)) {
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
        if !self.store_into_var(f, canon) {
            return false;
        }
        self.require_set(v, canon.unwrap_or(self.ty.never));
        match (site, canon, self.fn_sites.closure_return) {
            (Some(site), Some(e), _) => self.note_site_seed(site, e),
            // A closure's `return`: where a check without variables seeds it once the closure
            // is stored (`Ctx::bind_stored_fn`).
            (None, Some(e), Some(at)) => self.fn_sites.closure_rejects.push((at, e)),
            _ => {}
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

    /// The anonymous object defs and union defs whose fields or members hold error-set
    /// variables get the names a check without variables gives them, of their solved types:
    /// lowering names its layouts by them. `solved` is every type with variables and its
    /// solved form ([`Ctx::solved_types`]).
    pub(crate) fn name_solved_defs(&mut self, solved: &[(TyId, TyId)]) {
        if self.fn_sites.vars.is_empty() {
            return;
        }
        self.name_solved_fn_unions(solved);
        let n = self.info.len();
        let mut anons = vec![];
        for i in 0..n {
            let d = crate::hir::DefId(i as u32);
            let Some(a) = self.adt(d) else {
                continue;
            };
            let tys: Vec<(TyId, TyId)> = a.fields.iter().map(|f| (f.ty, f.declared)).collect();
            // Only a field typed by variables names the def otherwise than a check without
            // them: one holding them in a function type shows it as written either way.
            let anon = a.kind == crate::hir::AdtKind::Anon;
            if anon && (tys.iter()).any(|&(t, w)| self.typed_by_vars(t) || self.typed_by_vars(w)) {
                // A field typed by variables is its solved type; a function type with them is
                // shown as written, as a check without them shows an inferred one.
                let solve = |cx: &mut Self, t: TyId| match cx.typed_by_vars(t) {
                    true => cx.solved_type(t),
                    false => t,
                };
                let solved: Vec<(TyId, TyId)> = (tys.iter())
                    .map(|&(t, w)| (solve(self, t), solve(self, w)))
                    .collect();
                if let crate::defs::DefInfo::Adt(a) = &mut self.info[i] {
                    for (f, (t, w)) in a.fields.iter_mut().zip(solved) {
                        f.ty = t;
                        f.declared = w;
                    }
                }
                anons.push(d);
            }
        }
        let saved = std::mem::take(&mut self.display_params);
        let renamed: rustc_hash::FxHashSet<crate::hir::DefId> = anons.iter().copied().collect();
        for d in anons {
            let Some(a) = self.adt(d) else {
                continue;
            };
            let params = a.generics.names.clone();
            let fields: Vec<(String, bool, bool, TyId)> = (a.fields.iter())
                .map(|f| (f.name.clone(), f.readonly, f.optional, f.declared))
                .collect();
            self.display_params = params;
            let parts: Vec<String> = (fields.into_iter())
                .map(|(name, ro, opt, t)| {
                    let ro = if ro { "readonly " } else { "" };
                    let q = if opt { "?" } else { "" };
                    format!("{ro}{name}{q}: {}", self.display(t))
                })
                .collect();
            let name = format!("{{ {} }}", parts.join(", "));
            if let crate::defs::DefInfo::Adt(a) = &mut self.info[d.0 as usize] {
                a.name.clone_from(&name);
                a.qual_name.clone_from(&name);
            }
            if let Some(Some(crate::hir::Def::Adt(h))) = self.defs.get_mut(d.0 as usize) {
                h.name = name;
            }
        }
        for i in 0..n {
            let d = crate::hir::DefId(i as u32);
            let Some(e) = self.enum_info(d).filter(|e| e.is_union) else {
                continue;
            };
            let payloads: Vec<TyId> = (e.variants.iter())
                .filter_map(|v| v.payload.first().copied())
                .collect();
            let params = e.generics.names.clone();
            let holds = |cx: &mut Self, t: TyId| {
                cx.typed_by_vars(t)
                    || matches!(cx.ty.kind(t), TyKind::Adt(a, _) if renamed.contains(a))
            };
            if payloads.len() != e.variants.len() || !payloads.iter().any(|p| holds(self, *p)) {
                continue;
            }
            self.display_params = params;
            let names: Vec<String> = (payloads.iter())
                .map(|p| {
                    let s = match self.typed_by_vars(*p) {
                        true => self.solved_type(*p),
                        false => *p,
                    };
                    self.display(s)
                })
                .collect();
            let name = names.join(" | ");
            if let crate::defs::DefInfo::Enum(e) = &mut self.info[i] {
                for (v, n) in e.variants.iter_mut().zip(&names) {
                    v.name.clone_from(n);
                }
                e.qual_name.clone_from(&name);
                e.name.clone_from(&name);
            }
            if let Some(Some(crate::hir::Def::Enum(h))) = self.defs.get_mut(i) {
                for (v, n) in h.variants.iter_mut().zip(&names) {
                    v.name.clone_from(n);
                }
                h.name = name;
            }
        }
        self.display_params = saved;
    }

    /// A function type written without `throws` is shown as written, also solved: a check
    /// without variables shows the type it resolves to with its set so (`Types::inferred_fns`).
    /// The union defs whose members hold such a solved type, named when it was shown with its
    /// set (a union interned by solving, or one written with the same type), get the names a
    /// check without variables gives them.
    fn name_solved_fn_unions(&mut self, solved: &[(TyId, TyId)]) {
        let mut marked = rustc_hash::FxHashSet::default();
        for &(t, s) in solved {
            if t != s && self.shown_as_written(t) && self.ty.inferred_fns.insert(s) {
                marked.insert(s);
            }
        }
        if marked.is_empty() {
            return;
        }
        for i in 0..self.info.len() {
            let d = crate::hir::DefId(i as u32);
            let Some(e) = self
                .enum_info(d)
                .filter(|e| e.is_union && e.generics.names.is_empty())
            else {
                continue;
            };
            let payloads: Vec<TyId> = (e.variants.iter())
                .filter_map(|v| v.payload.first().copied())
                .collect();
            if payloads.len() != e.variants.len()
                || !payloads.iter().any(|p| self.holds_any(*p, &marked))
            {
                continue;
            }
            self.reorder_union_def(d, &payloads);
            let DefInfo::Enum(e) = &self.info[i] else {
                continue;
            };
            let (name, names): (String, Vec<String>) = (
                e.name.clone(),
                e.variants.iter().map(|v| v.name.clone()).collect(),
            );
            if let Some(Some(crate::hir::Def::Enum(h))) = self.defs.get_mut(i) {
                for (v, n) in h.variants.iter_mut().zip(names) {
                    v.name = n;
                }
                h.name = name;
            }
        }
    }

    /// Is function type `t` shown as written, without its error type (`crate::display`)?
    fn shown_as_written(&self, t: TyId) -> bool {
        let TyKind::FnPtr { ret, throws, .. } = self.ty.kind(t) else {
            return false;
        };
        self.ty.inferred_fns.contains(&t)
            || self.only_err_vars(*throws)
            || matches!(self.ty.kind(*ret), TyKind::Promise(_, e) if self.only_err_vars(*e))
    }

    /// Does `t` hold one of `tys` (itself or in its component types)?
    fn holds_any(&self, t: TyId, tys: &rustc_hash::FxHashSet<TyId>) -> bool {
        let mut seen = rustc_hash::FxHashSet::default();
        let mut stack = vec![t];
        while let Some(t) = stack.pop() {
            if tys.contains(&t) {
                return true;
            }
            if seen.insert(t) {
                stack.extend(crate::types::children(self.ty.kind(t)));
            }
        }
        false
    }

    /// Is `t` error-set variables, or a union (or option) holding them as members: a value's
    /// type made of them, not a function or promise type that has them as its error type?
    fn typed_by_vars(&mut self, t: TyId) -> bool {
        let t = self.ty.core(t).unwrap_or(t);
        if self.is_err_var(t) {
            return true;
        }
        let ms = self.union_members(t).unwrap_or_default();
        ms.iter().any(|m| self.is_err_var(*m))
    }

    /// `e` stored into variable `v` (a promise or a rejected value, not a closure's error):
    /// the members this adds are kept apart, since a check without variables seeds them where
    /// they were stored (`note_site_seed`), not into the site whose set `v` is.
    fn store_into_var(&mut self, v: TyId, e: Option<TyId>) -> bool {
        let Some(n) = self.err_var(v) else {
            return false;
        };
        let r = self.fn_sites.var_sets.find(n) as usize;
        let before = self.fn_sites.var_sets.members[r].len();
        if !self.throw_into_vars(Some(v), e) {
            return false;
        }
        let added: Vec<TyId> = self.fn_sites.var_sets.members[r][before..].to_vec();
        self.fn_sites
            .stored
            .extend(added.into_iter().map(|m| (n, m)));
        true
    }

    /// Each variable's set without the members only stores added (`store_into_var`): what a
    /// check without variables seeds the sites of the variable with (`Ctx::var_seeds`).
    pub(super) fn seed_sets(&mut self) -> rustc_hash::FxHashMap<TyId, Vec<TyId>> {
        let n = self.fn_sites.vars.len();
        let stored = std::mem::take(&mut self.fn_sites.stored);
        let stored: Vec<(usize, TyId)> = (stored.into_iter())
            .map(|(v, m)| (self.fn_sites.var_sets.find(v) as usize, m))
            .collect();
        let mut sets: Vec<Vec<TyId>> = vec![vec![]; n];
        let mut edges: Vec<Vec<usize>> = vec![vec![]; n];
        for v in 0..n {
            let r = self.fn_sites.var_sets.find(v as u32) as usize;
            if r != v {
                continue;
            }
            let only_stored = |m: &TyId| stored.iter().any(|(s, x)| x == m && *s == r);
            let ms = self
                .fn_sites
                .var_sets
                .members
                .get(r)
                .cloned()
                .unwrap_or_default();
            sets[r] = ms.into_iter().filter(|m| !only_stored(m)).collect();
            let es = self
                .fn_sites
                .var_sets
                .edges
                .get(r)
                .cloned()
                .unwrap_or_default();
            edges[r] = es
                .into_iter()
                .map(|e| self.fn_sites.var_sets.find(e) as usize)
                .collect();
        }
        loop {
            let mut changed = false;
            for r in 0..n {
                for e in edges[r].clone() {
                    for m in sets[e].clone() {
                        if !sets[r].contains(&m) {
                            sets[r].push(m);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let mut out = rustc_hash::FxHashMap::default();
        for v in 0..n {
            let r = self.fn_sites.var_sets.find(v as u32) as usize;
            let var = self
                .ty
                .intern(TyKind::Op(crate::hir::TyOp::ErrSite(v as u32), vec![]));
            let ms = self
                .error_union(&sets[r])
                .map(|t| self.error_members(t))
                .unwrap_or_default();
            out.insert(var, ms);
        }
        out
    }
}
