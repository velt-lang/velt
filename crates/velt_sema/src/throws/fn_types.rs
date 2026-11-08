//! Inferred error types of function types written without `throws` (https://veltlang.com/docs/reference/errors/
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
//! [`Portable`] type, classes and interfaces (`unknown` is one) by `DefId` and declaration span
//! (collected before any body is checked, so the same in every check of one program).

use rustc_hash::{FxHashMap, FxHashSet};

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
    /// A value of an interface type (`unknown`, `any`, a thrown interface value): the
    /// interface's id, declaration span and type arguments.
    Dyn(DefId, Span, Vec<Portable>),
    Array(Box<Portable>),
    Option(Box<Portable>, Nothing),
}

impl Portable {
    /// How many types it is made of.
    fn size(&self) -> usize {
        match self {
            Portable::Leaf(_) => 1,
            Portable::Def(_, _, args) | Portable::Dyn(_, _, args) => {
                1 + args.iter().map(Portable::size).sum::<usize>()
            }
            Portable::Array(e) | Portable::Option(e, _) => 1 + e.size(),
        }
    }
}

/// What each site throws, by the span of its written function type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FnTypeSeeds {
    pub(super) sites: FxHashMap<Span, Vec<Portable>>,
    /// The error types the previous check inferred for the functions declared before any body
    /// is checked, by `DefId` and declaration span: what a recursive call counts while the
    /// callee is still being checked ([`Ctx::recursion_hint`]).
    pub(super) hints: FxHashMap<DefId, (Span, Vec<Portable>)>,
    /// The error types the previous check inferred for interface method slots that a body read
    /// before the implementations converted to the interface were found (an object literal
    /// converted in code checked later), by interface `DefId` and slot, with the interface's
    /// declaration span: a call through the slot counts them from the start
    /// ([`Ctx::slot_hint`]).
    pub(super) slots: FxHashMap<(DefId, u32), (Span, Vec<Portable>)>,
    /// Check with the sets written in, without variables (`throws::vars`).
    no_vars: bool,
    /// The sites of std/proxy's `ProxyHandler` traps the previous check met.
    proxy_sites: FxHashSet<Span>,
    /// Check without any variable, also for the traps (`FnSites::doubted`).
    exact: bool,
}

/// The sites of one check: the sets it applies and what it found.
#[derive(Default)]
pub(crate) struct FnSites {
    /// The sets this check resolves its sites with.
    pub seeds: FnTypeSeeds,
    /// Every site resolved, with its type (in its own generic context).
    pub(super) sites: Vec<(Span, TyId)>,
    seen: FxHashSet<(Span, TyId)>,
    /// `sites` by file, as indices into it.
    pub(super) by_file: FxHashMap<velt_common::FileId, Vec<usize>>,
    /// `seeds` with what this check found added.
    pub(super) found: FnTypeSeeds,
    /// What a check without variables would have added to `found` for the stores the
    /// variables took (`promise_vars`): added to it when the program is checked again.
    pub(super) quiet: FxHashMap<Span, Vec<Portable>>,
    pub(super) grew: bool,
    /// A site's set grew (not only a hint for the next check).
    pub(super) sites_grew: bool,
    /// What a call through a function type may throw beyond its type ([`Ctx::indirect_error`]),
    /// for the current `found`.
    pub(super) indirect: FxHashMap<TyId, Option<TyId>>,
    /// The same for a call through a value read from a known site.
    pub(super) indirect_sites: FxHashMap<Span, Option<TyId>>,
    /// The written type of each parameter and field, by the span of its name.
    pub(super) written: FxHashMap<Span, Span>,
    /// The site the value being checked is stored into (an argument's parameter, a variable's
    /// or a field's type): what a closure created there throws goes into that site only.
    pub target: Option<Span>,
    /// The argument (by its span) of the `push` being checked and the site of its array's
    /// element type (`promise_sites`).
    pub push: Option<(Span, Span)>,
    /// Where each parameter's type is written, by the span of the function type written
    /// without `throws` it belongs to (`fn_flows`: the site of an argument of a call through a
    /// value of that type).
    pub(super) param_types: FxHashMap<Span, Vec<Span>>,
    /// The arguments (by their spans) of the call through a function value being checked, each
    /// with where its parameter's type is written.
    pub value_args: Vec<(Span, Span)>,
    /// The result type of an async function is being resolved: its `Promise<T>` rejects with
    /// what the body throws, so it is no site (`promise_sites`).
    pub async_ret: bool,
    /// Where a `Promise<T>` that gets an error-set variable is written (`promise_vars`): the
    /// result of a function type, a function or a closure. Elsewhere (a type argument, a
    /// method's result, a variable's type) a promise type is compared as written, so the sites
    /// there take their errors by checking again.
    pub promise_var_at: Option<Span>,
    /// The result type of a generator function is being resolved: its `Generator<T>` throws
    /// what the body throws, so it is no site (`generator_sites`).
    pub generator_ret: bool,
    /// The open error types of `new Promise` executors being checked and of
    /// `Promise.withResolvers` calls, with the site of each (`promise_sites`).
    pub rejects: Vec<(TyId, Span)>,
    /// Where the type of a field of an object literal's anonymous type is written in its value
    /// (`{ list: [] as Promise<T>[] }`), by def and field (the first literal of that shape).
    pub anon_written: FxHashMap<(DefId, u32), Span>,
    /// Where the body of a type alias is written, by the span of a use of the alias (`const r:
    /// Pages` with `type Pages = Record<string, Promise<string>>`): that use's sites are in the
    /// body (`Ctx::site_in`).
    pub alias_written: FxHashMap<Span, Span>,
    /// Async closures checked for a function type with a promise member in its result: the
    /// argument's span and the closure's (`promise_sites`).
    pub async_args: Vec<(Span, Span)>,
    /// The closure (by its span) whose `return` without a site is being checked, and what the
    /// promises returned there reject with that its type didn't allow (`promise_sites`).
    pub closure_return: Option<Span>,
    pub closure_rejects: Vec<(Span, TyId)>,
    /// The site each closure (by its span) was created for.
    pub(super) closure_sites: FxHashMap<Span, Span>,
    /// The closures that end in a rest parameter (`(...args) => …`): their function types do.
    pub rest_closures: FxHashSet<DefId>,
    /// Where the values of each site are stored (an argument read from a parameter), so what
    /// is stored into a site is stored into those too.
    pub(super) flows: FxHashMap<Span, Vec<Span>>,
    /// How many errors were added to `found`.
    pub added: usize,
    pub(super) last: bool,
    /// The check has error-set variables (`TyOp::ErrSite`): the program's own sites get one
    /// (`user_vars`), or the traps of std/proxy without a seeded set do. A trap's set grows
    /// through the program's sites and other traps (a `has` trap calling `Reflect.has` throws
    /// what `get` throws), so a variable for each, also in the checks with seeded sets, keeps a
    /// program whose traps throw at the number of checks it needs without proxies.
    pub use_vars: bool,
    /// The program's own sites get variables instead of seeded sets.
    pub user_vars: bool,
    /// The variable of each site, by its span, and the site of each variable.
    pub(super) var_of: FxHashMap<Span, TyId>,
    /// The variables of std/proxy's trap sites (`ProxyHandler`), by number.
    pub(super) trap_vars: FxHashSet<u32>,
    pub vars: Vec<Span>,
    /// The anonymous object defs replaced by the def of their solved shape before lowering
    /// (`Ctx::solved_anon_twins`).
    pub solved_anons: FxHashSet<DefId>,
    /// Each variable's solved error type, once solved (`never` for an empty set).
    pub solved: FxHashMap<TyId, TyId>,
    /// The constraints on the variables (`throws::vars`).
    pub(crate) var_sets: super::vars::VarSets,
    /// The members stores added to variables (`promise_vars`), by variable number.
    pub(crate) stored: Vec<(u32, TyId)>,
    /// How many `reject` values of promises whose rejection type is a variable were checked
    /// and not called (`var_known`): what they are called with is not counted.
    pub(crate) reject_values: usize,
    /// The closures created for sites' types found so far (`var_known`).
    pub(crate) site_closures: super::var_known::SiteClosures,
}

impl FnSites {
    /// The sites of a check with `seeds`; `last`: the program is not checked again whatever
    /// this check finds.
    pub fn new(seeds: FnTypeSeeds, last: bool) -> Self {
        let allowed = std::env::var_os("VELT_NO_SITE_VARS").is_none();
        let user_vars = seeds.sites.is_empty() && !seeds.no_vars && allowed;
        let trap_vars = allowed && !seeds.exact && !seeds.proxy_sites.is_empty();
        FnSites {
            use_vars: user_vars || trap_vars,
            user_vars,
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
        let quiet = std::mem::take(&mut self.quiet);
        merge_seeds(&mut self.found, quiet, &self.flows);
    }

    /// The check with error-set variables reported errors: the sets to check the program with
    /// again without variables (as written, the sets found by checking again), whose
    /// diagnostics are the program's, whatever types they show (`throws::vars`). Not when the
    /// program is checked again anyway, with the sets this check found: that check has no
    /// variables of the program's sites.
    pub fn doubted(&self, errors: bool) -> Option<FnTypeSeeds> {
        (self.use_vars && errors && !self.again()).then(|| {
            let mut s = self.found.clone();
            merge_seeds(&mut s, self.quiet.clone(), &self.flows);
            s.no_vars = true;
            s.exact = true;
            s
        })
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
        let params: Vec<TyId> = match self.type_level.callable_params.contains(&span) {
            true => (written.iter())
                .map(|t| self.resolve_param_form(t, env))
                .collect(),
            false => written.iter().map(|t| self.resolve_type(t, env)).collect(),
        };
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
        let never = self.ty.never;
        let ret_span = ret.span;
        let throws_clause = throws;
        let err = throws.map(|t| {
            let e = self.resolve_type(t, env);
            self.no_void_error(e, t.span);
            e
        });
        // `Promise<T>` with a written `throws E`: the promise rejects with `E` and nothing else
        // (https://veltlang.com/docs/reference/async/ "Errors"), so `E` is what an argument's promise rejects with,
        // not one member of a union with the inferred rejections of a `Promise<T>` site.
        let mut ret = match (promise_payload_written(ret), err) {
            (Some(payload), Some(e)) => {
                let v = self.resolve_type(payload, env);
                let e = self.canon_error(Some(e)).unwrap_or(never);
                self.ty.promise_rejecting(v, e)
            }
            _ => self.resolve_promise_var_at(ret, env),
        };
        let inferred = throws.is_none();
        // A trap of std/proxy gets a variable in every check with variables, unless the previous
        // check seeded its set; the program's own sites only in a check without seeds.
        let gets_var = match self.scopes.get(env.module).is_some_and(|s| s.is_std) {
            true => self.fn_sites.use_vars && !self.fn_sites.seeds.sites.contains_key(&span),
            false => self.fn_sites.user_vars,
        };
        // A `Promise<T>` result written without an error type has a variable of its own
        // (`promise_sites`): the function type's errors are what its promise rejects with, so
        // the function type's site shares it.
        let ret_var = match self.ty.kind(ret) {
            TyKind::Promise(_, e) if gets_var && self.fn_sites.var_of.get(&ret_span) == Some(e) => {
                Some(*e)
            }
            _ => None,
        };
        let err = match (err, ret_var) {
            (Some(e), _) => self.canon_error(Some(e)),
            (None, Some(v)) => {
                self.fn_sites.var_of.insert(span, v);
                Some(v)
            }
            (None, None) if gets_var => self.site_var(span, env.module),
            (None, None) => self.site_error(span, env.module),
        };
        let var_site = inferred && err.is_some() && gets_var;
        let inferred = inferred && err.is_some() && !gets_var;
        let err = err.unwrap_or(never);
        let plain_ret = ret;
        // A function returning a promise or a generator rejects with its errors there.
        let throws = match self.rejection_error(ret) {
            Some(e) if err != never => {
                let e = self.join_errors(Some(e), Some(err)).unwrap_or(never);
                ret = self.with_rejection_error(ret, e);
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
            if self.site_module(env.module) && span != Span::DUMMY {
                let at: Vec<Span> = written.iter().map(|w| w.span).collect();
                self.fn_sites.param_types.entry(span).or_insert(at);
            }
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
        let rejects = self.rejection_error(ret).is_some_and(|e| e != never);
        if throws != never || rejects {
            return t;
        }
        let Some(err) = self.site_error(span, module) else {
            self.record_fn_site(span, module, t);
            return t;
        };
        let throws = match self.rejection_error(ret) {
            Some(_) => {
                ret = self.with_rejection_error(ret, err);
                never
            }
            None => err,
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
    pub(super) fn site_var(&mut self, span: Span, module: usize) -> Option<TyId> {
        if !self.site_module(module) || span == Span::DUMMY {
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
        if self.scopes.get(module).is_some_and(|s| s.is_std) {
            self.fn_sites.trap_vars.insert(id);
        }
        Some(v)
    }

    /// A function type without `throws` written at `span` in module `module`, resolved to
    /// `params => ret`: what it throws (`never` when nothing was stored into it that throws),
    /// recording the site.
    pub(crate) fn site_error(&mut self, span: Span, module: usize) -> Option<TyId> {
        if !self.site_module(module) {
            return None;
        }
        let members = self.fn_sites.seeds.sites.get(&span)?.clone();
        let ms: Vec<TyId> = members.iter().filter_map(|p| self.unport(p)).collect();
        self.error_union(&ms)
    }

    /// Are the function types written without `throws` in module `module` sites: those of a
    /// user module, and the traps of std/lang/proxy's `ProxyHandler` (the only function types
    /// those modules write), so a trap may throw: a proxy operation throws what its trap throws
    /// (hir_encodings.md "Proxy and Reflect").
    fn site_module(&self, module: usize) -> bool {
        match self.scopes.get(module) {
            None => false,
            Some(s) if s.is_std => self.modules.get(module).is_some_and(|m| {
                crate::body::expr::proxy::PROXY_MODULES.contains(&m.path.as_str())
            }),
            Some(_) => true,
        }
    }

    /// Record `t` as the type of the site at `span` (a user module's function type written
    /// without `throws`).
    pub(super) fn record_fn_site(&mut self, span: Span, module: usize, t: TyId) {
        if !self.site_module(module) || span == Span::DUMMY {
            return;
        }
        if self.scopes.get(module).is_some_and(|s| s.is_std) {
            self.fn_sites.found.proxy_sites.insert(span);
        }
        if self.fn_sites.seen.insert((span, t)) {
            let i = self.fn_sites.sites.len();
            self.fn_sites.sites.push((span, t));
            self.fn_sites.by_file.entry(span.file).or_default().push(i);
        }
    }

    /// A closure throwing `err` is used as a value of function type `used_as`, which does not
    /// allow it: add `err` to the sites `used_as` may come from. False when no site matches or
    /// `err` can't be kept (then the use is reported). Any thrown type another check can hold is
    /// kept (`null`, a string, a class), as JavaScript may throw any value.
    pub(crate) fn seed_fn_sites(&mut self, used_as: TyId, err: TyId, site: Option<Span>) -> bool {
        let mut members = vec![];
        for m in self.error_members(err) {
            match self.port_thrown(m) {
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
            if self.rejection_error(*ret).is_some() {
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
            (TyKind::Adt(..), TyKind::Adt(..))
                if self.generator_site_matches(g, used, m).is_some() =>
            {
                self.generator_site_matches(g, used, m) == Some(true)
            }
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

    /// `t` as a [`Portable`] type kept in an inferred error set: as in JavaScript any thrown
    /// value (`null`, a string, a class, `Error | null`), but only a small type, since the sets
    /// reach every call through their sites.
    pub(super) fn port_thrown(&self, t: TyId) -> Option<Portable> {
        let p = self.port(t, 0)?;
        (p.size() <= 8).then_some(p)
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
    pub(super) fn port(&self, t: TyId, depth: u32) -> Option<Portable> {
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
            | TyKind::Never
            | TyKind::Literal(_) => Portable::Leaf(k.clone()),
            TyKind::Adt(d, args) if self.collected(*d) => {
                let span = *self.def_spans.get(d.0 as usize)?;
                let args = (args.iter())
                    .map(|a| self.port(*a, depth + 1))
                    .collect::<Option<Vec<_>>>()?;
                Portable::Def(*d, span, args)
            }
            TyKind::Dyn(d, args) if self.collected(*d) => {
                let span = *self.def_spans.get(d.0 as usize)?;
                let args = (args.iter())
                    .map(|a| self.port(*a, depth + 1))
                    .collect::<Option<Vec<_>>>()?;
                Portable::Dyn(*d, span, args)
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
            Portable::Dyn(d, span, args) => {
                if self.def_spans.get(d.0 as usize) != Some(span) {
                    return None;
                }
                let args = (args.iter())
                    .map(|a| self.unport(a))
                    .collect::<Option<Vec<_>>>()?;
                TyKind::Dyn(*d, args)
            }
            Portable::Array(e) => TyKind::Array(self.unport(e)?),
            Portable::Option(e, n) => TyKind::Option(self.unport(e)?, *n),
        };
        Some(self.ty.intern(k))
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
        let promise_err = |cx: &mut Self, t: TyId| match cx.rejection_error(t) {
            Some(e) => {
                let never = cx.ty.never;
                match cx.ty.kind(t) {
                    TyKind::Promise(v, _) => (*v, Some(e)),
                    _ => (cx.with_rejection_error(t, never), Some(e)),
                }
            }
            None => (t, None),
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

/// Add the sets `more` to `seeds`, and to the sites their values flow to (as
/// `Ctx::add_to_site` does).
fn merge_seeds(
    seeds: &mut FnTypeSeeds,
    more: FxHashMap<Span, Vec<Portable>>,
    flows: &FxHashMap<Span, Vec<Span>>,
) {
    let mut more: Vec<(Span, Vec<Portable>)> = more.into_iter().collect();
    more.sort_by_key(|(s, _)| (s.file, s.lo, s.hi));
    for (s, ms) in more {
        let mut work = vec![s];
        while let Some(s) = work.pop() {
            let set = seeds.sites.entry(s).or_default();
            let mut grew = false;
            for m in &ms {
                if !set.contains(m) {
                    set.push(m.clone());
                    grew = true;
                }
            }
            if grew {
                work.extend(flows.get(&s).into_iter().flatten().copied());
            }
        }
    }
}

/// `T` of a result type written `Promise<T>` (no error type).
fn promise_payload_written(t: &ast::TypeExpr) -> Option<&ast::TypeExpr> {
    match &t.kind {
        ast::TypeExprKind::Named { path, args }
            if path.len() == 1 && path[0].name == "Promise" && args.len() == 1 =>
        {
            args.first()
        }
        _ => None,
    }
}
