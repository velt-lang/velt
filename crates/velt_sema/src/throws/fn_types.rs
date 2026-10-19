//! Inferred error types of function types written without `throws` (docs/reference/errors.md
//! "Dynamic calls").
//!
//! Every function type written without a `throws` clause in a user module is a *site*. When a
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
    sites: HashMap<Span, Vec<Portable>>,
}

/// The sites of one check: the sets it applies and what it found.
#[derive(Default)]
pub(crate) struct FnSites {
    /// The sets this check resolves its sites with.
    pub seeds: FnTypeSeeds,
    /// Every site resolved, with its type (in its own generic context).
    sites: Vec<(Span, TyId)>,
    seen: HashSet<(Span, TyId)>,
    /// `seeds` with what this check found added.
    found: FnTypeSeeds,
    grew: bool,
}

impl FnSites {
    pub fn new(seeds: FnTypeSeeds) -> Self {
        FnSites {
            found: seeds.clone(),
            seeds,
            ..Default::default()
        }
    }

    /// The sets to check the program with again, when this check added to them.
    pub fn grown(&self) -> Option<FnTypeSeeds> {
        self.grew.then(|| self.found.clone())
    }
}

impl Ctx<'_> {
    /// `(params) => ret [throws E]`. After a `Promise` result, `throws E` is the promise's
    /// rejection type (calling an async function never throws; awaiting its promise does).
    /// Without `throws` in a user module, it throws what the throwing closures stored into it
    /// throw (the module docs).
    pub(crate) fn resolve_fn_type(
        &mut self,
        params: &[ast::TypeExpr],
        ret: &ast::TypeExpr,
        throws: Option<&ast::TypeExpr>,
        env: &crate::resolve::TyEnv,
        span: Span,
    ) -> TyId {
        let params = params.iter().map(|t| self.resolve_type(t, env)).collect();
        let mut ret = self.resolve_type(ret, env);
        let never = self.ty.never;
        let throws_clause = throws;
        let err = throws.map(|t| {
            let e = self.resolve_type(t, env);
            self.no_void_error(e, t.span);
            e
        });
        let err = match err {
            Some(e) => self.canon_error(Some(e)),
            None => self.site_error(span, env.module),
        };
        let err = err.unwrap_or(never);
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
        });
        if throws_clause.is_none() {
            self.record_fn_site(span, env.module, t);
        }
        t
    }

    /// A function type without `throws` written at `span` in module `module`, resolved to
    /// `params => ret`: what it throws (`never` when nothing was stored into it that throws),
    /// recording the site.
    fn site_error(&mut self, span: Span, module: usize) -> Option<TyId> {
        if self.scopes.get(module).is_none_or(|s| s.is_std) {
            return None;
        }
        let members = self.fn_sites.seeds.sites.get(&span)?.clone();
        let ms: Vec<TyId> = members.iter().filter_map(|p| self.unport(p)).collect();
        self.error_union(&ms)
    }

    /// Record `t` as the type of the site at `span` (a user module's function type written
    /// without `throws`).
    fn record_fn_site(&mut self, span: Span, module: usize, t: TyId) {
        if self.scopes.get(module).is_none_or(|s| s.is_std) || span == Span::DUMMY {
            return;
        }
        if self.fn_sites.seen.insert((span, t)) {
            self.fn_sites.sites.push((span, t));
        }
    }

    /// A closure throwing `err` is used as a value of function type `used_as`, which does not
    /// allow it: add `err` to the sites `used_as` may come from. False when no site matches or
    /// `err` can't be kept (then the use is reported). Only `Error` classes are kept: the sets
    /// reach every function type of the same shape, and a `catch` around a call through one
    /// reads `e.message` as TypeScript code does.
    pub(crate) fn seed_fn_sites(&mut self, used_as: TyId, err: TyId) -> bool {
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
        let matching: Vec<Span> = (self.fn_sites.sites.iter())
            .filter(|(_, g)| self.site_matches(*g, used_as, 0))
            .map(|(s, _)| *s)
            .collect();
        for &s in &matching {
            let set = self.fn_sites.found.sites.entry(s).or_default();
            for p in &members {
                if !set.contains(p) {
                    set.push(p.clone());
                    self.fn_sites.grew = true;
                }
            }
        }
        !matching.is_empty()
    }

    /// Can a value of `used` have been typed by site type `g`: equal, with `g`'s type
    /// parameters (and deferred types) matching anything, and error types ignored.
    fn site_matches(&self, g: TyId, used: TyId, depth: u32) -> bool {
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
            (TyKind::Param(_) | TyKind::Op(..) | TyKind::Error, _) | (_, TyKind::Error) => true,
            (
                TyKind::FnPtr {
                    params: p, ret: r, ..
                },
                TyKind::FnPtr {
                    params: q, ret: s, ..
                },
            ) => all(p, q) && m(*r, *s),
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
    fn unport(&mut self, p: &Portable) -> Option<TyId> {
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
            },
            TyKind::FnPtr {
                params: q,
                ret: s,
                throws: f,
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
        if p != q || rv != sv || re.is_some() != se.is_some() || (e == f && re == se) {
            return None;
        }
        let have = self.join_errors(Some(e), re);
        let want = self.join_errors(Some(f), se);
        if self.error_outside(want, have).is_none() {
            return Some(true);
        }
        if let Some(have) = have {
            self.seed_fn_sites(to, have);
        }
        Some(false)
    }
}
