//! The whole program's effects, and the search for an assignment a right-hand side may run.

use std::collections::HashSet;

use super::implicit::{Implicit, Reach};
use super::{adt_kind, field_types, has_params, Effects, Frame, Layer, MAX_VISITS};
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{AdtKind, DefId, TyId, TyKind};

/// Every type nested in the type arguments the program passes and in those of the types it
/// uses.
fn type_args_closure(cx: &Ctx, mut work: Vec<TyId>) -> HashSet<TyId> {
    for i in 0..cx.ty.table.len() {
        match cx.ty.kind(TyId(i as u32)) {
            TyKind::Adt(_, args) | TyKind::Dyn(_, args) => work.extend(args),
            _ => {}
        }
    }
    let mut out = HashSet::new();
    while let Some(t) = work.pop() {
        if out.insert(t) {
            work.extend(parts(cx, t));
        }
    }
    out
}

/// The types `t` is built from.
fn parts(cx: &Ctx, t: TyId) -> Vec<TyId> {
    match cx.ty.kind(t) {
        TyKind::Adt(_, a) | TyKind::Dyn(_, a) | TyKind::Tuple(a) => a.clone(),
        TyKind::Array(x) | TyKind::Option(x, _) | TyKind::Shared(x) => vec![*x],
        TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => vec![*a, *b],
        TyKind::FnPtr {
            params,
            ret,
            throws,
        } => params.iter().copied().chain([*ret, *throws]).collect(),
        _ => vec![],
    }
}

/// Every function's effects, and the functions that may run through a function value or a
/// method call.
pub(super) struct Program {
    pub(super) fns: Vec<Option<Effects>>,
    /// Functions used as values anywhere.
    pub(super) values: Vec<DefId>,
    /// Methods (functions with a `this`), by member name.
    pub(super) methods: Vec<(String, DefId)>,
    /// The type arguments the program passes, and every type nested in them or in those of the
    /// types it uses ([`type_args_closure`], computed when first needed).
    pub(super) type_args: (Vec<TyId>, std::cell::OnceCell<HashSet<TyId>>),
    /// Types shared values hold inline ([`close_shared`]).
    pub(super) shared: HashSet<TyId>,
    /// The code that runs without a call ([`Implicit`]).
    pub(super) im: Implicit,
}

impl Program {
    /// Does lowering pin a holder of type `t`: a shared object type it can count?
    pub(super) fn pinned(&self, cx: &Ctx, t: TyId) -> bool {
        self.shared.contains(&t)
            && !has_params(cx, t)
            && matches!(cx.ty.kind(t), TyKind::Adt(d, _) if adt_kind(cx, *d) == Some(AdtKind::Anon))
    }

    /// May running the right-hand side `rhs` (its direct effects) replace one of `path`'s
    /// objects, directly or in what it calls?
    pub(super) fn may_assign(&self, cx: &mut Ctx, rhs: &Effects, tys: &[Layer]) -> bool {
        // A variable no other function can reach: only the right-hand side itself assigns it.
        if tys
            .iter()
            .any(|l| l.local.is_some_and(|x| rhs.locals.contains(&x)))
        {
            return true;
        }
        let tys: Vec<Layer> = tys.iter().filter(|l| l.local.is_none()).copied().collect();
        let tys = &tys[..];
        if tys.is_empty() {
            return false;
        }
        if rhs.anything {
            return self.hits(cx, rhs, &None, tys)
                || self
                    .fns
                    .iter()
                    .flatten()
                    .any(|fx| self.hits(cx, fx, &None, tys));
        }
        let mut seen: HashSet<(DefId, Option<Vec<TyId>>)> = HashSet::new();
        let mut work: Vec<(&Effects, Option<Vec<TyId>>)> = vec![(rhs, None)];
        let mut values = false;
        let mut names: HashSet<Option<String>> = HashSet::new();
        let (mut resumed, mut open): (bool, Option<Reach>) = (false, None);
        while let Some((fx, frame)) = work.pop() {
            if self.hits(cx, fx, &frame, tys) {
                return true;
            }
            let mut next: Vec<(DefId, Option<Vec<TyId>>)> = Vec::new();
            // The hooks glue and drops call, and the generator bodies a resume runs.
            for (set, dispose) in [(&fx.drops, true), (&fx.glue, false)] {
                for &t in set {
                    let args = self.type_args_all(cx);
                    let r = self.im.resolve(cx, t, &frame, args, &mut open);
                    next.extend(self.im.hooks_of(&r, dispose).into_iter().map(|d| (d, None)));
                }
            }
            if fx.resumes && !resumed {
                resumed = true;
                next.extend(self.im.generators.iter().map(|d| (*d, None)));
            }
            for (d, f) in &fx.calls {
                let f = match f {
                    Frame::Inherit => frame.clone(),
                    Frame::Known(args) => Some(match &frame {
                        Some(outer) => args.iter().map(|a| cx.subst(*a, outer)).collect(),
                        None => args.clone(),
                    }),
                    Frame::Unknown => None,
                };
                next.push((*d, f));
            }
            if fx.indirect && !values {
                values = true;
                next.extend(self.values.iter().map(|d| (*d, None)));
            }
            for m in &fx.methods {
                if names.contains(&None) || !names.insert(m.clone()) {
                    continue;
                }
                let of = self
                    .methods
                    .iter()
                    .filter(|(n, _)| m.as_ref().is_none_or(|m| m == n));
                next.extend(of.map(|(_, d)| (*d, None)));
            }
            for (d, f) in next {
                if seen.len() >= MAX_VISITS {
                    return true;
                }
                if let Some(Some(callee)) = self.fns.get(d.0 as usize) {
                    if seen.insert((d, f.clone())) {
                        work.push((callee, f));
                    }
                }
            }
        }
        false
    }

    /// Every type the program passes as a type argument ([`type_args_closure`]).
    fn type_args_all(&self, cx: &Ctx) -> &HashSet<TyId> {
        let (passed, all) = &self.type_args;
        all.get_or_init(|| type_args_closure(cx, passed.clone()))
    }

    /// Does `fx`, run with type arguments `frame` (`None`: not known), replace one of `path`'s
    /// objects: assign its field (a whole value of its type, for one not held in a field), or a
    /// generic field that may be it?
    fn hits(&self, cx: &mut Ctx, fx: &Effects, frame: &Option<Vec<TyId>>, path: &[Layer]) -> bool {
        let direct = |l: &Layer| match l.field {
            Some((key, loose)) => fx.fields.contains(&key) || fx.loose.contains(&loose),
            None => fx.assigns.contains(&l.ty),
        };
        if path.iter().any(direct) {
            return true;
        }
        for l in path.iter().filter(|l| l.generic) {
            let assigns: Vec<TyId> = fx.assigns.iter().copied().collect();
            if assigns.into_iter().any(|a| self.instance(cx, l.ty, a, 0)) {
                return true;
            }
        }
        fx.generic.iter().any(|&g| {
            let g = match frame {
                Some(args) => cx.subst(g, args),
                None => g,
            };
            if has_params(cx, g) {
                path.iter().any(|l| self.instance(cx, g, l.ty, 0))
            } else {
                path.iter().any(|l| l.ty == g)
            }
        })
    }

    /// Can `p`, a type mentioning type parameters, be `t` once instantiated with types the
    /// program uses as type arguments?
    fn instance(&self, cx: &mut Ctx, p: TyId, t: TyId, depth: u32) -> bool {
        if depth > 8 {
            return true;
        }
        if !has_params(cx, p) {
            return p == t;
        }
        match (cx.ty.kind(p).clone(), cx.ty.kind(t).clone()) {
            (TyKind::Param(_), _) => self.type_args_all(cx).contains(&t),
            (TyKind::Adt(d, a), TyKind::Adt(e, b)) if d == e => self.pairs(cx, &a, &b, depth),
            (TyKind::Adt(d, a), TyKind::Adt(e, b))
                if adt_kind(cx, d) == Some(AdtKind::Anon)
                    && adt_kind(cx, e) == Some(AdtKind::Anon) =>
            {
                // The same shape: generic `{ a: T }` and the concrete `{ a: string }`.
                let (Some(DefInfo::Adt(x)), Some(DefInfo::Adt(y))) =
                    (cx.info.get(d.0 as usize), cx.info.get(e.0 as usize))
                else {
                    return false;
                };
                if x.fields.len() != y.fields.len()
                    || x.fields
                        .iter()
                        .zip(&y.fields)
                        .any(|(f, g)| f.name != g.name)
                {
                    return false;
                }
                let fields: Vec<(TyId, TyId)> = x
                    .fields
                    .iter()
                    .zip(&y.fields)
                    .map(|(f, g)| (f.ty, g.ty))
                    .collect();
                fields.into_iter().all(|(f, g)| {
                    let (f, g) = (cx.subst(f, &a), cx.subst(g, &b));
                    self.instance(cx, f, g, depth + 1)
                })
            }
            (TyKind::Option(x, _), TyKind::Option(y, _)) => self.instance(cx, x, y, depth + 1),
            (TyKind::Tuple(a), TyKind::Tuple(b)) => self.pairs(cx, &a, &b, depth),
            _ => false,
        }
    }

    fn pairs(&self, cx: &mut Ctx, a: &[TyId], b: &[TyId], depth: u32) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(x, y)| self.instance(cx, *x, *y, depth + 1))
    }
}

/// The types shared values hold inline: a shared object literal type that is never changed in
/// place is copied field by field, sharing what its fields hold.
pub(super) fn close_shared(cx: &mut Ctx, mut work: Vec<TyId>) -> HashSet<TyId> {
    let assigned = crate::assigned_fields::assigned_objects(cx);
    let mut out = HashSet::new();
    while let Some(t) = work.pop() {
        if !out.insert(t) {
            continue;
        }
        match cx.ty.kind(t).clone() {
            TyKind::Option(x, _) => work.push(x),
            TyKind::Tuple(ts) => work.extend(ts),
            TyKind::Adt(d, args) => {
                let kind = adt_kind(cx, d);
                if kind == Some(AdtKind::Class) || assigned.contains(&d) {
                    continue;
                }
                for f in field_types(cx, d) {
                    let f = cx.subst(f, &args);
                    work.push(f);
                }
            }
            _ => {}
        }
    }
    out
}
