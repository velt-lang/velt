//! The functions an opaque `with` callback may be (`super::opaque`): every closure and named
//! function of the program whose parameter types are the callback's — a generic one through
//! each instantiation of the function making it — except closures written as `with`'s argument
//! itself, which cannot be anything else. Each is checked as if `with` called it, without
//! changing it (`super::would_cross`); the call is accepted when none of them stores across the
//! lock or makes a promise from the value (#457).

use std::collections::{HashMap, HashSet};

use velt_common::Span;

use super::summary::Summaries;
use super::values::Resolver;
use crate::ctx::Ctx;
use crate::defs::{BodyState, FnKind};
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, TyId, TyKind};
use crate::visit;

/// Concrete type arguments of a function, and whether every instantiation was found.
type Instances = (Vec<Vec<TyId>>, bool);

/// Each function's direct calls: the calling body and the type arguments.
type CallIndex = HashMap<DefId, Vec<(DefId, Vec<TyId>)>>;

/// Where a candidate crosses the lock.
#[derive(Clone, Copy)]
pub(super) enum Crossing {
    /// A promise made from the locked value.
    Promise(Span),
    /// A store of a part of the value outside it, or of an outside object into it.
    Store(Span),
}

/// A candidate that crosses the lock: the function, and where.
pub(super) struct Offender {
    pub(super) def: DefId,
    pub(super) at: Crossing,
}

/// Finds and checks the candidates, caching what it learns.
pub(super) struct Candidates<'s> {
    s: &'s Summaries,
    /// Closures passed to `with` as its argument itself.
    direct: HashSet<DefId>,
    /// The checked candidates (with their instantiations) and what they do.
    checked: HashMap<(DefId, Vec<TyId>), Option<Crossing>>,
    /// The direct calls of each function and its uses as a value: the using body and the type
    /// arguments.
    calls: Option<CallIndex>,
    /// The functions that may be candidates at all (one parameter, synchronous, …).
    fns: Option<Vec<DefId>>,
    /// The result of [`Candidates::offender`] per parameter type.
    offenders: HashMap<TyId, Option<(DefId, Crossing)>>,
    /// The result of [`Candidates::instances`] per function making a candidate.
    instances: HashMap<DefId, Instances>,
}

impl<'s> Candidates<'s> {
    pub(super) fn new(s: &'s Summaries, direct: HashSet<DefId>) -> Self {
        Candidates {
            s,
            direct,
            checked: HashMap::new(),
            calls: None,
            fns: None,
            offenders: HashMap::new(),
            instances: HashMap::new(),
        }
    }

    /// The first function given a `param` that crosses the lock.
    pub(super) fn offender(
        &mut self,
        cx: &mut Ctx,
        res: &mut Resolver,
        param: TyId,
    ) -> Option<Offender> {
        if let Some(found) = self.offenders.get(&param) {
            return found.map(|(def, at)| Offender { def, at });
        }
        let direct = &self.direct;
        let fns = self
            .fns
            .get_or_insert_with(|| {
                cx.fn_defs
                    .iter()
                    .copied()
                    .filter(|d| {
                        let info = cx.fn_info(*d);
                        info.state == BodyState::Done
                            && matches!(info.kind, FnKind::Closure | FnKind::Free | FnKind::Static)
                            && !info.is_async
                            && !info.is_generator
                            && info.params.len() == 1
                            && !direct.contains(d)
                    })
                    .collect()
            })
            .clone();
        let found = self.first_offender(cx, res, &fns, param);
        self.offenders.insert(param, found);
        found.map(|(def, at)| Offender { def, at })
    }

    fn first_offender(
        &mut self,
        cx: &mut Ctx,
        res: &mut Resolver,
        fns: &[DefId],
        param: TyId,
    ) -> Option<(DefId, Crossing)> {
        for &d in fns {
            let ty = cx.fn_info(d).params[0].ty;
            for targs in self.takes(cx, res, d, ty, param) {
                if let Some(at) = self.check(cx, res, d, targs) {
                    return Some((d, at));
                }
            }
        }
        None
    }

    /// The instantiations in which function `d`, whose parameter has type `ty`, takes a
    /// `param`: the type arguments of the function making it (none when `ty` is `param`, or
    /// when an instantiation is not known).
    fn takes(
        &mut self,
        cx: &mut Ctx,
        res: &mut Resolver,
        d: DefId,
        ty: TyId,
        param: TyId,
    ) -> Vec<Vec<TyId>> {
        if ty == param {
            return vec![vec![]];
        }
        if !cx.mentions_params(ty) {
            return vec![];
        }
        let (all, complete) = self.instances(cx, res, d);
        let mut out: Vec<Vec<TyId>> = all
            .into_iter()
            .filter(|targs| cx.ty.subst(ty, targs) == param)
            .collect();
        // An instantiation not found through direct calls (a method called through an
        // interface, …): any type of the same shape, in the program's own code, checked as
        // written.
        let own = !cx.scopes[cx.fn_info(d).module].is_std;
        if !complete && own && same_shape(cx, ty, param) {
            out.push(vec![]);
        }
        out
    }

    /// What candidate `d` (instantiated with `targs`) does across the lock (checked once).
    fn check(
        &mut self,
        cx: &mut Ctx,
        res: &mut Resolver,
        d: DefId,
        targs: Vec<TyId>,
    ) -> Option<Crossing> {
        let key = (d, targs);
        if let Some(c) = self.checked.get(&key) {
            return *c;
        }
        let c = match cx.fn_info(d).kind {
            FnKind::Closure => would_cross(cx, self.s, res, d, &key.1),
            // A named function has nothing captured to store into (`super::named_callback`).
            _ => self
                .s
                .get(d)
                .is_some_and(|s| s.promises & 1 != 0)
                .then(|| Crossing::Promise(cx.fn_info(d).name_span)),
        };
        self.checked.insert(key, c);
        c
    }

    /// The concrete type arguments the function making `d` gets through its direct calls, and
    /// those of its callers while they still depend on type parameters; false when some
    /// instantiation is not found that way.
    fn instances(&mut self, cx: &mut Ctx, res: &mut Resolver, d: DefId) -> Instances {
        let start = maker(cx, res, d);
        if let Some(found) = self.instances.get(&start) {
            return found.clone();
        }
        let found = self.find_instances(cx, res, start);
        self.instances.insert(start, found.clone());
        found
    }

    fn find_instances(&mut self, cx: &mut Ctx, res: &mut Resolver, start: DefId) -> Instances {
        let mut out = vec![];
        let mut complete = true;
        let mut seen = HashSet::new();
        // `None`: the maker's own type parameters.
        let mut work: Vec<(DefId, Option<Vec<TyId>>)> = vec![(start, None)];
        while let Some((f, so_far)) = work.pop() {
            if seen.len() > MAX_STEPS {
                // A recursive function instantiating itself with ever larger types.
                return (out, false);
            }
            if !seen.insert((f, so_far.clone())) {
                continue;
            }
            let calls = self.calls_of(cx, f);
            if calls.is_empty() {
                complete = false;
            }
            let generics = cx.fn_info(f).generics.len();
            for (caller, targs) in calls {
                if targs.len() < generics {
                    // A use whose type arguments are not all known.
                    complete = false;
                    continue;
                }
                let sub: Vec<TyId> = match &so_far {
                    None => targs,
                    Some(ts) => ts.iter().map(|t| cx.ty.subst(*t, &targs)).collect(),
                };
                if sub.iter().any(|t| cx.mentions_params(*t)) {
                    work.push((maker(cx, res, caller), Some(sub)));
                } else if !out.contains(&sub) {
                    out.push(sub);
                }
            }
        }
        (out, complete)
    }

    fn calls_of(&mut self, cx: &mut Ctx, f: DefId) -> Vec<(DefId, Vec<TyId>)> {
        let calls = self.calls.get_or_insert_with(|| direct_calls(cx));
        calls.get(&f).cloned().unwrap_or_default()
    }
}

/// How many functions [`Candidates::instances`] follows before giving up.
const MAX_STEPS: usize = 256;

/// The named function whose type parameters a closure's types use: the one it is made in.
fn maker(cx: &mut Ctx, res: &mut Resolver, mut d: DefId) -> DefId {
    while cx.fn_info(d).kind == FnKind::Closure {
        match res.parent(cx, d) {
            Some(p) => d = p,
            None => break,
        }
    }
    d
}

/// The direct calls of every function of the program, and its uses as a value (`const f =
/// wrap;` instantiates `wrap` as a call would).
fn direct_calls(cx: &mut Ctx) -> CallIndex {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    let mut out = CallIndex::new();
    for caller in fns {
        let Some(Def::Fn(mut body)) = cx.defs[caller.0 as usize].take() else {
            continue;
        };
        visit::exprs_mut(&mut body.body.block, &mut |e: &mut Expr| match &e.kind {
            E::Call {
                callee: Callee::Def(g, targs),
                ..
            }
            | E::FnRef(g, targs) => out.entry(*g).or_default().push((caller, targs.clone())),
            _ => {}
        });
        cx.defs[caller.0 as usize] = Some(Def::Fn(body));
    }
    out
}

/// Can `generic` become `concrete` by substituting its type parameters?
fn same_shape(cx: &Ctx, generic: TyId, concrete: TyId) -> bool {
    if generic == concrete || matches!(cx.ty.kind(generic), TyKind::Param(_)) {
        return true;
    }
    let (g, c) = (cx.ty.kind(generic), cx.ty.kind(concrete));
    let same_head = match (g, c) {
        (TyKind::Adt(a, _), TyKind::Adt(b, _)) | (TyKind::Dyn(a, _), TyKind::Dyn(b, _)) => a == b,
        _ => std::mem::discriminant(g) == std::mem::discriminant(c),
    };
    let (gs, cs) = (crate::types::children(g), crate::types::children(c));
    same_head
        && !gs.is_empty()
        && gs.len() == cs.len()
        && gs.iter().zip(&cs).all(|(a, b)| same_shape(cx, *a, *b))
}

/// What closure `c` would do across the lock if `with` called it, without changing it
/// (`candidates`): where it makes a promise from the value, or else where it stores across
/// the lock (a store that would need a copy, or one that cannot have one). `targs`: the type
/// arguments of the function making it (an instantiation), or none.
pub(super) fn would_cross(
    cx: &mut Ctx,
    s: &Summaries,
    res: &mut Resolver,
    c: DefId,
    targs: &[TyId],
) -> Option<Crossing> {
    let (mut bodies, resolved, named) = super::take_bodies(cx, res, c);
    let originals = bodies.clone();
    for (_, f) in bodies.iter_mut() {
        instantiate(cx, f, targs);
    }
    let before = transfers(&mut bodies);
    let found = super::crossings(cx, s, c, &mut bodies, resolved, named);
    let mut after = transfers(&mut bodies);
    for (d, f) in originals {
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
    }
    if let Some(m) = found.made.first() {
        return Some(Crossing::Promise(super::made_span(m)));
    }
    for span in before {
        if let Some(i) = after.iter().position(|x| *x == span) {
            after.swap_remove(i);
        }
    }
    let stored = found.unfixable.iter().map(|u| u.span).chain(after);
    stored.min_by_key(|s| (s.file.0, s.lo)).map(Crossing::Store)
}

/// Body `f` with its types' type parameters replaced by `targs`.
fn instantiate(cx: &mut Ctx, f: &mut FnDef, targs: &[TyId]) {
    if targs.is_empty() {
        return;
    }
    for l in f.body.locals.iter_mut() {
        l.ty = cx.ty.subst(l.ty, targs);
    }
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        e.ty = cx.ty.subst(e.ty, targs);
        if let E::Call {
            callee: Callee::Def(_, args),
            ..
        } = &mut e.kind
        {
            for a in args.iter_mut() {
                *a = cx.ty.subst(*a, targs);
            }
        }
    });
}

/// The spans of the transferred values in `bodies`.
fn transfers(bodies: &mut [(DefId, FnDef)]) -> Vec<Span> {
    let mut out = vec![];
    for (_, f) in bodies.iter_mut() {
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Call {
                callee: Callee::Intrinsic(Intrinsic::Transfer),
                ..
            } = &e.kind
            {
                out.push(e.span);
            }
        });
    }
    out
}
