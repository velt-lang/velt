//! Inferred error types of function types written without `throws` (docs/reference/errors.md
//! "Dynamic calls").
//!
//! Every function type written without a `throws` clause in a user module is a *site*, and so
//! is each trap of std/proxy's `ProxyHandler` (a trap may throw, as in JavaScript). When a
//! closure that throws is used as a value of a function type that does not allow its errors,
//! the sites that type may come from (matched structurally, a site's type parameters matching
//! anything, error parts ignored) get the closure's errors ([`Ctx::seed_fn_sites`]). When some
//! site's set grew, the program is checked again (`crate::check_on_current_thread`) with each
//! site resolved as if `throws <its set>` were written there (`Ctx::resolve_type`), until no set
//! grows. A program that stores no throwing closure into such a type is checked once.
//!
//! The sets outlive one check, whose interned types they can't use: an error type is kept as a
//! [`Portable`] type, classes by `DefId` and declaration span (collected before any body is
//! checked, so the same in every check of one program).

use std::collections::{HashMap, HashSet};

use velt_common::Span;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{DefId, Nothing, TyId, TyKind};

/// A type independent of one check's type table.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Portable {
    /// A type without component types (`string`, `i32`, a literal type).
    Leaf(TyKind),
    /// A class (or other definition) instance: its id, declaration span and type arguments.
    Def(DefId, Span, Vec<Portable>),
    Array(Box<Portable>),
    Option(Box<Portable>, Nothing),
}

/// What each site throws, by the span of its written function type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FnTypeSeeds {
    pub(super) sites: HashMap<Span, Vec<Portable>>,
    /// The error types the previous check inferred for the functions declared before any body
    /// is checked, by `DefId` and declaration span: what a recursive call counts while the
    /// callee is still being checked ([`Ctx::recursion_hint`]).
    hints: HashMap<DefId, (Span, Vec<Portable>)>,
    /// Check with the sets written in, without variables (`throws::vars`).
    no_vars: bool,
}

/// The sites of one check: the sets it applies and what it found.
#[derive(Default)]
pub(crate) struct FnSites {
    /// The sets this check resolves its sites with.
    pub seeds: FnTypeSeeds,
    /// Every site resolved, with its type (in its own generic context).
    pub(super) sites: Vec<(Span, TyId)>,
    seen: HashSet<(Span, TyId)>,
    /// `sites` by file, as indices into it.
    pub(super) by_file: HashMap<velt_common::FileId, Vec<usize>>,
    /// `seeds` with what this check found added.
    pub(super) found: FnTypeSeeds,
    pub(super) grew: bool,
    /// What a call through a function type may throw beyond its type ([`Ctx::indirect_error`]),
    /// for the current `found`.
    pub(super) indirect: HashMap<TyId, Option<TyId>>,
    /// The same for a call through a value read from a known site.
    pub(super) indirect_sites: HashMap<Span, Option<TyId>>,
    /// The written type of each parameter and field, by the span of its name.
    pub(super) written: HashMap<Span, Span>,
    /// The site the value being checked is stored into (an argument's parameter, a variable's
    /// or a field's type): what a closure created there throws goes into that site only.
    pub target: Option<Span>,
    /// The site each closure (by its span) was created for.
    pub(super) closure_sites: HashMap<Span, Span>,
    /// The closures that end in a rest parameter (`(...args) => …`): their function types do.
    pub rest_closures: HashSet<DefId>,
    /// Where the values of each site are stored (an argument read from a parameter), so what
    /// is stored into a site is stored into those too.
    pub(super) flows: HashMap<Span, Vec<Span>>,
    /// How many errors were added to `found`.
    pub added: usize,
    last: bool,
    /// Sites get an error-set variable (`TyOp::ErrSite`) instead of a seeded set.
    pub use_vars: bool,
    /// The variable of each site, by its span, and the site of each variable.
    pub(super) var_of: HashMap<Span, TyId>,
    pub vars: Vec<Span>,
    /// Each variable's solved error type, once solved (`never` for an empty set).
    pub solved: HashMap<TyId, TyId>,
    /// The constraints on the variables (`throws::vars`).
    pub(crate) var_sets: super::vars::VarSets,
    /// A type with a variable failed a comparison or a bound: when the check reports errors,
    /// they may come from the variables ([`FnSites::doubted`]).
    pub doubt: std::cell::Cell<bool>,
}

impl FnSites {
    /// The sites of a check with `seeds`; `last`: the program is not checked again whatever
    /// this check finds.
    pub fn new(seeds: FnTypeSeeds, last: bool) -> Self {
        FnSites {
            use_vars: seeds.sites.is_empty()
                && !seeds.no_vars
                && std::env::var_os("VELT_NO_SITE_VARS").is_none(),
            found: seeds.clone(),
            seeds,
            last,
            ..Default::default()
        }
    }

    /// The sets to check the program with again, when this check added to them.
    pub fn grown(&self) -> Option<FnTypeSeeds> {
        (self.grew && !self.last).then(|| self.found.clone())
    }

    /// The variables could not express what this check found: check again with their solved
    /// sets written in (`throws::vars`).
    pub(super) fn fallback(&mut self) {
        self.found.no_vars = true;
        self.grew = true;
    }

    /// The check with error-set variables reported errors, and a type with a variable failed
    /// a comparison: the sets to check the program with again without variables (as written,
    /// the sets found by checking again), whose errors are the program's.
    pub fn doubted(&self, errors: bool) -> Option<FnTypeSeeds> {
        (self.use_vars && self.doubt.get() && errors).then(|| {
            let mut s = self.found.clone();
            s.no_vars = true;
            s
        })
    }

    /// A type with a variable failed a comparison: the errors are in doubt
    /// ([`FnSites::doubted`]).
    pub fn set_doubt(&self) {
        self.doubt.set(true);
    }

    /// Is this the last check, whatever it finds?
    pub fn is_last(&self) -> bool {
        self.last
    }

    /// Will the program be checked again (the passes after throws inference can be skipped)?
    pub fn again(&self) -> bool {
        self.grew && !self.last
    }
}

impl Ctx<'_> {
    /// `(params) => ret [throws E]`, with `rest`: `(a: A, ...xs: T[]) => R`, whose last parameter
    /// must have an array or tuple type. After a `Promise` result, `throws E` is the promise's
    /// rejection type (calling an async function never throws; awaiting its promise does).
    /// Without `throws` in a user module, it throws what the throwing closures stored into it
    /// throw (the module docs).
    pub(crate) fn resolve_fn_type(
        &mut self,
        written: &[ast::TypeExpr],
        rest: bool,
        ret: &ast::TypeExpr,
        throws: Option<&ast::TypeExpr>,
        env: &crate::resolve::TyEnv,
        span: Span,
    ) -> TyId {
        let params: Vec<TyId> = written.iter().map(|t| self.resolve_type(t, env)).collect();
        if let (true, Some(&last), Some(w)) = (rest, params.last(), written.last()) {
            // A tuple is its elements (`Types::intern`); a type parameter or `infer` slot is
            // one once it is known.
            let later = matches!(
                self.ty.kind(last),
                TyKind::Tuple(_) | TyKind::Param(_) | TyKind::Op(..)
            );
            if !later && !crate::collect::sigs::rest_param_type(self, w.span, last) {
                return self.ty.error;
            }
        }
        let mut ret = self.resolve_type(ret, env);
        let never = self.ty.never;
        let throws_clause = throws;
        let err = throws.map(|t| {
            let e = self.resolve_type(t, env);
            self.no_void_error(e, t.span);
            e
        });
        let inferred = throws.is_none();
        let err = match err {
            Some(e) => self.canon_error(Some(e)),
            None if self.fn_sites.use_vars => self.site_var(span, env.module),
            None => self.site_error(span, env.module),
        };
        let var_site = inferred && err.is_some() && self.fn_sites.use_vars;
        let inferred = inferred && err.is_some() && !self.fn_sites.use_vars;
        let err = err.unwrap_or(never);
        let plain_ret = ret;
        let throws = match self.ty.kind(ret).clone() {
            TyKind::Promise(v, e) if err != never => {
                let e = self.join_errors(Some(e), Some(err)).unwrap_or(never);
                ret = self.ty.promise_rejecting(v, e);
                never
            }
            _ => err,
        };
        let t = self.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        });
        if throws_clause.is_none() {
            self.record_fn_site(span, env.module, t);
        }
        // The type as written is interned here too, as in a check without variables: unions
        // order their members by it (`throws::vars`).
        if var_site {
            let TyKind::FnPtr { params, rest, .. } = self.ty.kind(t).clone() else {
                unreachable!("ICE: a function type");
            };
            let plain = self.ty.intern(TyKind::FnPtr {
                params,
                ret: plain_ret,
                throws: never,
                rest,
            });
            self.fn_sites.var_sets.note_written(t, plain);
        }
        if inferred {
            self.ty.inferred_fns.insert(t);
        }
        t
    }

    /// The type of a `let` declared at `span` in module `module` without a type, from its
    /// initializer's type `t`: a function type is a site too (it may be assigned a closure that
    /// throws), as if written there.
    pub(crate) fn untyped_let_site(&mut self, t: TyId, span: Span, module: usize) -> TyId {
        let TyKind::FnPtr {
            params,
            mut ret,
            throws,
            rest,
        } = self.ty.kind(t).clone()
        else {
            return t;
        };
        let never = self.ty.never;
        let rejects = matches!(self.ty.kind(ret), TyKind::Promise(_, e) if *e != never);
        if throws != never || rejects {
            return t;
        }
        let Some(err) = self.site_error(span, module) else {
            self.record_fn_site(span, module, t);
            return t;
        };
        let throws = match self.ty.kind(ret).clone() {
            TyKind::Promise(v, _) => {
                ret = self.ty.promise_rejecting(v, err);
                never
            }
            _ => err,
        };
        let s = self.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        });
        self.record_fn_site(span, module, s);
        self.ty.inferred_fns.insert(s);
        s
    }

    /// The error-set variable of the site at `span` in module `module` (`None` outside user
    /// modules).
    fn site_var(&mut self, span: Span, module: usize) -> Option<TyId> {
        // std/proxy's sites take seeded sets only: a program whose traps throw nothing keeps
        // its types (and code) as they were.
        if self.scopes.get(module).is_none_or(|s| s.is_std) || span == Span::DUMMY {
            return None;
        }
        if let Some(&v) = self.fn_sites.var_of.get(&span) {
            return Some(v);
        }
        let id = self.fn_sites.vars.len() as u32;
        let v = self
            .ty
            .intern(TyKind::Op(crate::hir::TyOp::ErrSite(id), vec![]));
        self.fn_sites.vars.push(span);
        self.fn_sites.var_of.insert(span, v);
        Some(v)
    }

    /// A function type without `throws` written at `span` in module `module`, resolved to
    /// `params => ret`: what it throws (`never` when nothing was stored into it that throws),
    /// recording the site.
    fn site_error(&mut self, span: Span, module: usize) -> Option<TyId> {
        if !self.site_module(module) {
            return None;
        }
        let members = self.fn_sites.seeds.sites.get(&span)?.clone();
        let ms: Vec<TyId> = members.iter().filter_map(|p| self.unport(p)).collect();
        self.error_union(&ms)
    }

    /// Are the function types written without `throws` in module `module` sites: those of a
    /// user module, and the traps of std/proxy's `ProxyHandler` (the only function types that
    /// module writes), so a trap may throw: a proxy operation throws what its trap throws
    /// (hir_encodings.md "Proxy and Reflect").
    fn site_module(&self, module: usize) -> bool {
        match self.scopes.get(module) {
            None => false,
            Some(s) if s.is_std => self.modules.get(module).is_some_and(|m| m.path == "std/proxy"),
            Some(_) => true,
        }
    }

    /// Record `t` as the type of the site at `span` (a user module's function type written
    /// without `throws`).
    fn record_fn_site(&mut self, span: Span, module: usize, t: TyId) {
        if !self.site_module(module) || span == Span::DUMMY {
            return;
        }
        if self.fn_sites.seen.insert((span, t)) {
            let i = self.fn_sites.sites.len();
            self.fn_sites.sites.push((span, t));
            self.fn_sites.by_file.entry(span.file).or_default().push(i);
        }
    }

    /// A closure throwing `err` is used as a value of function type `used_as`, which does not
    /// allow it: add `err` to the sites `used_as` may come from. False when no site matches or
    /// `err` can't be kept (then the use is reported). Only `Error` classes are kept: the sets
    /// reach every function type of the same shape, and a value that is no `Error` (`null`)
    /// would reach `catch` blocks and standard library callbacks that never see it.
    pub(crate) fn seed_fn_sites(&mut self, used_as: TyId, err: TyId, site: Option<Span>) -> bool {
        let mut members = vec![];
        for m in self.error_members(err) {
            if !self.is_error_class(m) {
                return false;
            }
            match self.port(m, 0) {
                Some(p) => members.push(p),
                None => return false,
            }
        }
        // Stored into a known site: that one, and the sites its values flow to.
        if let Some(s) = site {
            self.add_to_site(s, &members);
            return true;
        }
        let matching: Vec<Span> = (self.fn_sites.sites.iter())
            .filter(|(_, g)| self.site_matches(*g, used_as, 0))
            .map(|(s, _)| *s)
            .collect();
        for &s in &matching {
            self.add_to_site(s, &members);
        }
        !matching.is_empty()
    }

    /// What a call through a value of function type `t` throws beyond what `t` says: what is
    /// stored into the sites `t` may come from, as found so far (`None` before anything is).
    /// The error types of the functions calling through such a type grow with them, and so
    /// do the sets of the sites those functions are stored into: the sets are propagated in
    /// the inference of one check (`throws::infer_all`), and the program is checked again
    /// once, with them.
    pub(crate) fn indirect_error(&mut self, t: TyId, site: Option<Span>) -> Option<TyId> {
        if self.fn_sites.found.sites.is_empty() {
            return None;
        }
        // A function returning a promise rejects with what is stored into its sites (its type
        // has those errors from the next check on); a call does not throw them.
        if let TyKind::FnPtr { ret, .. } = self.ty.kind(t) {
            if matches!(self.ty.kind(*ret), TyKind::Promise(..)) {
                return None;
            }
        }
        if let Some(s) = site {
            return self.site_set(s);
        }
        if let Some(e) = self.fn_sites.indirect.get(&t) {
            return *e;
        }
        let matching: Vec<Span> = (self.fn_sites.sites.iter())
            .filter(|(_, g)| self.site_matches(*g, t, 0))
            .map(|(s, _)| *s)
            .collect();
        let mut members = vec![];
        for s in matching {
            for p in self
                .fn_sites
                .found
                .sites
                .get(&s)
                .cloned()
                .unwrap_or_default()
            {
                members.extend(self.unport(&p));
            }
        }
        let e = self.error_union(&members);
        self.fn_sites.indirect.insert(t, e);
        e
    }

    /// Keep the error types this check inferred for the next check, when there is one.
    pub(crate) fn record_recursion_hints(&mut self, fns: &[DefId]) {
        if !self.fn_sites.again() {
            return;
        }
        for &d in fns {
            if d.0 as usize >= self.collected_defs {
                continue;
            }
            let Some(t) = self.fn_info(d).throws else {
                continue;
            };
            let ms: Option<Vec<Portable>> = (self.error_members(t).into_iter())
                .map(|m| self.port(m, 0))
                .collect();
            if let Some(ms) = ms {
                let span = self.def_spans[d.0 as usize];
                self.fn_sites.found.hints.insert(d, (span, ms));
            }
        }
    }

    /// What the previous check inferred function `d` throws (see [`FnTypeSeeds::hints`]).
    pub(crate) fn recursion_hint(&mut self, d: DefId) -> Option<TyId> {
        let (span, ms) = self.fn_sites.seeds.hints.get(&d)?.clone();
        if self.def_spans.get(d.0 as usize) != Some(&span) {
            return None;
        }
        let ms: Vec<TyId> = ms.iter().filter_map(|p| self.unport(p)).collect();
        self.error_union(&ms)
    }

    /// Can a value of `used` have been typed by site type `g`: equal, with `g`'s type
    /// parameters (and deferred types) matching anything, and error types ignored. A part of
    /// `used` that is a reported type error matches nothing: it would match every site.
    pub(super) fn site_matches(&self, g: TyId, used: TyId, depth: u32) -> bool {
        if g == used {
            return true;
        }
        if depth > 32 {
            return false;
        }
        let m = |a: TyId, b: TyId| self.site_matches(a, b, depth + 1);
        let all =
            |a: &[TyId], b: &[TyId]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| m(*x, *y));
        match (self.ty.kind(g), self.ty.kind(used)) {
            (_, TyKind::Error) => false,
            (TyKind::Param(_) | TyKind::Op(..) | TyKind::Error, _) => true,
            (
                TyKind::FnPtr {
                    params: p,
                    ret: r,
                    rest: pr,
                    ..
                },
                TyKind::FnPtr {
                    params: q,
                    ret: s,
                    rest: qr,
                    ..
                },
            ) => pr == qr && all(p, q) && m(*r, *s),
            (TyKind::Promise(a, _), TyKind::Promise(b, _)) => m(*a, *b),
            (TyKind::Adt(d, xs), TyKind::Adt(e, ys)) | (TyKind::Dyn(d, xs), TyKind::Dyn(e, ys)) => {
                d == e && all(xs, ys)
            }
            (TyKind::Array(a), TyKind::Array(b)) | (TyKind::Shared(a), TyKind::Shared(b)) => {
                m(*a, *b)
            }
            (TyKind::Option(a, n), TyKind::Option(b, o)) => n == o && m(*a, *b),
            (TyKind::Map(a, x), TyKind::Map(b, y)) => m(*a, *b) && m(*x, *y),
            (TyKind::Tuple(xs), TyKind::Tuple(ys)) => all(xs, ys),
            _ => false,
        }
    }

    /// `t` as a [`Portable`] type, for the seeds of another check.
    pub(super) fn port_type(&self, t: TyId) -> Option<Portable> {
        self.port(t, 0)
    }

    /// Add `members` to the seeds of site `s`.
    pub(super) fn add_seed(&mut self, s: Span, members: Vec<Portable>) {
        self.add_to_site(s, &members);
    }

    /// `t` as a [`Portable`] type (`None`: it has parts that only exist in this check).
    fn port(&self, t: TyId, depth: u32) -> Option<Portable> {
        if depth > 16 {
            return None;
        }
        let k = self.ty.kind(t);
        Some(match k {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Str
            | TyKind::Unit
            | TyKind::Symbol
            | TyKind::Literal(_) => Portable::Leaf(k.clone()),
            TyKind::Adt(d, args) if (d.0 as usize) < self.collected_defs => {
                let span = *self.def_spans.get(d.0 as usize)?;
                let args = (args.iter())
                    .map(|a| self.port(*a, depth + 1))
                    .collect::<Option<Vec<_>>>()?;
                Portable::Def(*d, span, args)
            }
            TyKind::Array(e) => Portable::Array(Box::new(self.port(*e, depth + 1)?)),
            TyKind::Option(e, n) => Portable::Option(Box::new(self.port(*e, depth + 1)?), *n),
            _ => return None,
        })
    }

    /// A [`Portable`] type in this check (`None`: its class is not the same here).
    pub(super) fn unport(&mut self, p: &Portable) -> Option<TyId> {
        let k = match p {
            Portable::Leaf(k) => k.clone(),
            Portable::Def(d, span, args) => {
                if self.def_spans.get(d.0 as usize) != Some(span) {
                    return None;
                }
                let args = (args.iter())
                    .map(|a| self.unport(a))
                    .collect::<Option<Vec<_>>>()?;
                TyKind::Adt(*d, args)
            }
            Portable::Array(e) => TyKind::Array(self.unport(e)?),
            Portable::Option(e, n) => TyKind::Option(self.unport(e)?, *n),
        };
        Some(self.ty.intern(k))
    }

    /// Is `t` the prelude's `Error` or a class extending it?
    fn is_error_class(&mut self, t: TyId) -> bool {
        let Some(root) = self.prelude_adt("Error") else {
            return false;
        };
        let mut cur = t;
        for _ in 0..64 {
            if matches!(self.ty.kind(cur), TyKind::Adt(d, _) if *d == root) {
                return true;
            }
            match self.base_of(cur) {
                Some(b) => cur = b,
                None => return false,
            }
        }
        false
    }

    /// `from` and `to` are function types that differ only in what they throw: `Some(true)`
    /// when `to` allows every error of `from` (a value converts through a wrapper), else
    /// `Some(false)`, after adding `from`'s errors to the sites `to` may come from (a value of a
    /// function type stored into one written without `throws`).
    pub(crate) fn fn_throws_differ(&mut self, from: TyId, to: TyId) -> Option<bool> {
        let (
            TyKind::FnPtr {
                params: p,
                ret: r,
                throws: e,
                rest: pr,
            },
            TyKind::FnPtr {
                params: q,
                ret: s,
                throws: f,
                rest: qr,
            },
        ) = (self.ty.kind(from).clone(), self.ty.kind(to).clone())
        else {
            return None;
        };
        let promise_err = |cx: &Self, t: TyId| match cx.ty.kind(t) {
            TyKind::Promise(v, e) => (*v, Some(*e)),
            _ => (t, None),
        };
        let ((rv, re), (sv, se)) = (promise_err(self, r), promise_err(self, s));
        if p != q || pr != qr || rv != sv || re.is_some() != se.is_some() || (e == f && re == se) {
            return None;
        }
        let have = self.join_errors(Some(e), re);
        let want = self.join_errors(Some(f), se);
        if self.error_outside(want, have).is_none() {
            return Some(true);
        }
        if let Some(have) = have {
            let site = self.target_site(to);
            self.seed_fn_sites(to, have, site);
        }
        Some(false)
    }
}
