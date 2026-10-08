//! The methods a value can run without a direct call: those reached by dynamic dispatch through
//! an interface (`Program::impls`) or a base class (vtable slots). The instantiation checks
//! (`record_keys`, `json`) propagate a generic function's requirements to its callers through
//! `Callee::Def` type arguments; a dispatched method has no such call, so its requirements are
//! instantiated wherever its class type is mentioned with concrete arguments instead.
//!
//! Those checks are fixed points over the functions ([`Rounds`]), and their work is counted
//! ([`instantiation_work`]) so a test can tell that it grows linearly with the program.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ctx::Ctx;
use crate::hir::{DefId, TyId, TyKind};
use crate::types::{children, collect_params};

/// A method and the type arguments it runs with (its owner's, for a class method; the
/// interface's followed by the implementor, for an interface's default method).
pub(crate) type Target = (DefId, Vec<TyId>);

/// [`Dispatch::targets`] per type, memoized.
#[derive(Default)]
pub(crate) struct Dispatch {
    memo: HashMap<TyId, Vec<Target>>,
    /// Indices into `Ctx::impls` per implementing struct or class, built on first use.
    impls_by_adt: Option<HashMap<DefId, Vec<usize>>>,
    /// Vtable entries and impls examined: see [`instantiation_work`].
    pub(crate) work: u64,
}

impl Dispatch {
    /// Every dynamically dispatchable method of a struct or class type `t` mentions (also
    /// nested, as in `G<i64>[]`), instantiated with that type's arguments.
    pub(crate) fn targets(&mut self, cx: &mut Ctx, t: TyId) -> Vec<Target> {
        if let Some(ts) = self.memo.get(&t) {
            return ts.clone();
        }
        let mut out = vec![];
        if let TyKind::Adt(d, args) = cx.ty.kind(t).clone() {
            self.adt_targets(cx, d, args, &mut out);
        }
        for c in children(&cx.ty.kind(t).clone()) {
            for x in self.targets(cx, c) {
                if !out.contains(&x) {
                    out.push(x);
                }
            }
        }
        self.memo.insert(t, out.clone());
        out
    }

    /// The vtable entries and interface implementations of `Adt(d, args)` and its base classes.
    fn adt_targets(&mut self, cx: &mut Ctx, d: DefId, args: Vec<TyId>, out: &mut Vec<Target>) {
        let chain = class_chain(cx, d, args);
        let Some(vtable) = cx.adt(d).map(|a| a.vtable.clone()) else {
            return;
        };
        self.work += vtable.len() as u64;
        for m in vtable {
            if let Some(a) = owner_args(cx, &chain, m) {
                push(out, (m, a));
            }
        }
        let by_adt = self.impls_by_adt.get_or_insert_with(|| impls_by_adt(cx));
        // The impls of every class in the chain, in program order.
        let mut impls: Vec<usize> = chain
            .iter()
            .flat_map(|(c, _)| by_adt.get(c).into_iter().flatten().copied())
            .collect();
        impls.sort_unstable();
        impls.dedup();
        self.work += impls.len() as u64;
        for i in impls {
            let (ty, iface_args, methods) = {
                let imp = &cx.impls[i];
                (imp.ty, imp.iface_args.clone(), imp.methods.clone())
            };
            let TyKind::Adt(c, _) = *cx.ty.kind(ty) else {
                continue;
            };
            let Some((_, cargs)) = chain.iter().find(|(x, _)| *x == c).cloned() else {
                continue;
            };
            for m in methods {
                let a = if cx.iface(owner(cx, m)).is_some() {
                    let mut a: Vec<TyId> =
                        iface_args.iter().map(|t| cx.subst(*t, &cargs)).collect();
                    a.push(cx.ty.intern(TyKind::Adt(c, cargs.clone())));
                    Some(a)
                } else {
                    owner_args(cx, &chain, m)
                };
                if let Some(a) = a {
                    push(out, (m, a));
                }
            }
        }
    }
}

/// A requirement `need` of a method (in terms of its type params) instantiated with `args`;
/// `None` if it mentions the method's own type params (those are only instantiated by direct,
/// generic calls).
pub(crate) fn instantiate(cx: &mut Ctx, need: TyId, args: &[TyId]) -> Option<TyId> {
    let mut ps = vec![];
    collect_params(&cx.ty, need, &mut ps);
    ps.iter()
        .all(|p| (*p as usize) < args.len())
        .then(|| cx.subst(need, args))
}

/// `Ctx::impls` indices per implementing struct or class.
fn impls_by_adt(cx: &Ctx) -> HashMap<DefId, Vec<usize>> {
    let mut by_adt: HashMap<DefId, Vec<usize>> = HashMap::new();
    for (i, imp) in cx.impls.iter().enumerate() {
        if let TyKind::Adt(c, _) = *cx.ty.kind(imp.ty) {
            by_adt.entry(c).or_default().push(i);
        }
    }
    by_adt
}

fn push(out: &mut Vec<Target>, t: Target) {
    if !out.contains(&t) {
        out.push(t);
    }
}

/// `(d, args)` followed by its base classes with their (substituted) arguments.
fn class_chain(cx: &mut Ctx, d: DefId, args: Vec<TyId>) -> Vec<(DefId, Vec<TyId>)> {
    let mut chain = vec![(d, args)];
    while chain.len() < 64 {
        let (c, a) = chain
            .last()
            .cloned()
            .expect("ICE: class chain is never empty");
        let Some(base) = cx.adt(c).and_then(|x| x.base) else {
            break;
        };
        let base = cx.subst(base, &a);
        let Some(next) = cx.class_of(base) else { break };
        chain.push(next);
    }
    chain
}

fn owner(cx: &Ctx, m: DefId) -> DefId {
    match &cx.info[m.0 as usize] {
        crate::defs::DefInfo::Fn(f) => f.owner.unwrap_or(m),
        _ => m,
    }
}

/// The arguments of method `m`'s owning class in `chain`.
fn owner_args(cx: &Ctx, chain: &[(DefId, Vec<TyId>)], m: DefId) -> Option<Vec<TyId>> {
    let o = owner(cx, m);
    chain.iter().find(|(c, _)| *c == o).map(|(_, a)| a.clone())
}

/// A fixed point over the functions of a program (`0..n`, in definition order): the same
/// sequence of visits as rounds that each visit every function in order until a round changes
/// nothing, minus the visits that could not change anything, those of functions none of whose
/// inputs changed since their last visit. A change made while visiting function `i` is seen by
/// its readers after `i` in the same round and by the others in the next round, as in a full
/// round, so the checks find the same requirements in the same order.
pub(crate) struct Rounds {
    current: BTreeSet<usize>,
    next: BTreeSet<usize>,
    /// The function being visited.
    at: usize,
    /// The functions that read each def's requirements.
    readers: HashMap<DefId, Vec<usize>>,
}

impl Rounds {
    /// Every function is visited in the first round; `reads`: (function, def it reads).
    pub(crate) fn new(n: usize, reads: impl IntoIterator<Item = (usize, DefId)>) -> Rounds {
        let mut readers: HashMap<DefId, Vec<usize>> = HashMap::new();
        for (f, d) in reads {
            let rs = readers.entry(d).or_default();
            if rs.last() != Some(&f) {
                rs.push(f);
            }
        }
        Rounds {
            current: (0..n).collect(),
            next: BTreeSet::new(),
            at: 0,
            readers,
        }
    }

    /// The next function to visit.
    pub(crate) fn pop(&mut self) -> Option<usize> {
        if self.current.is_empty() {
            std::mem::swap(&mut self.current, &mut self.next);
        }
        self.at = self.current.pop_first()?;
        Some(self.at)
    }

    /// The requirements of `d` grew while visiting the current function.
    pub(crate) fn changed(&mut self, d: DefId) {
        for &r in self.readers.get(&d).into_iter().flatten() {
            if r > self.at {
                self.current.insert(r);
            } else {
                self.next.insert(r);
            }
        }
    }
}

/// The instantiation checks whose work [`instantiation_work`] counts.
#[derive(Clone, Copy)]
pub(crate) enum Pass {
    RecordKeys,
    Json,
    PromiseCopies,
}

static WORK: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Adds `n` units of work to `pass`'s counter.
pub(crate) fn add_work(pass: Pass, n: u64) {
    WORK[pass as usize].fetch_add(n, Ordering::Relaxed);
}

/// The work the instantiation checks did in this process so far (all checks on all threads): per
/// function visit the call sites, closures and types it evaluated, plus the vtable entries and
/// impls examined for dynamic dispatch. A deterministic cost measure: tests compare it between
/// program sizes to tell that each check grows linearly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstantiationWork {
    pub record_keys: u64,
    pub json: u64,
    pub promise_copies: u64,
}

/// See [`InstantiationWork`].
pub fn instantiation_work() -> InstantiationWork {
    let get = |p: Pass| WORK[p as usize].load(Ordering::Relaxed);
    InstantiationWork {
        record_keys: get(Pass::RecordKeys),
        json: get(Pass::Json),
        promise_copies: get(Pass::PromiseCopies),
    }
}
