//! The sets of error-set variables as known while bodies are checked (`throws::vars`): what a
//! `catch` variable's type counts for the variables in what its `try` body throws.
//!
//! A variable's set is solved after every body is checked, but a `catch` variable's type is
//! needed where it is checked. It takes the set the constraints recorded so far give: the
//! error types stored into the variable, the variables it contains, and what the closures
//! created for its site whose bodies are checked throw now (which `throws::checks` adds once
//! inference is done). The check assumes the variable's final set is that one; when it is not
//! (a closure checked later, a set that grew), the program is checked again with the solved
//! sets, as for a variable assumed empty, which is the set known before anything is stored.
//!
//! What is known depends on the order bodies are checked in: a `catch` in a function whose body
//! is checked before the code that stores the handlers (an `async function` called before them,
//! or checked on demand from an earlier call) knows nothing yet, so such a program is checked
//! twice, as before.
//!
//! A value whose type is variables (the parameter of a `.catch` callback, a settled result's
//! `reason`) is checked as that type, which is nothing until the sets are solved: such a value
//! is right only while nothing is known to be stored into them, and else the program is
//! checked again with the solved sets, where it has its type. So is a program that uses the
//! `reject` of a promise whose rejection type is a variable as a value, whose calls through
//! that value are not counted.
//!
//! A promise type shown with a variable as its rejection type (in a diagnostic: `Promise<T>`)
//! shows it as nothing, where a check without variables shows its set: that assumes the set
//! empty, and else the program is checked again with the solved sets, whose diagnostics show
//! it. A union def's name does not count: it is named again once the sets are solved.
//!
//! What the checked closures throw is kept with the variable they throw into as they are found,
//! so finding a variable's known set costs what the variables it reaches hold.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::defs::{BodyState, FnKind};
use crate::hir::{DefId, TyId, TyKind};

/// The closures created for a site's type (their error type holds its variable), found so far.
#[derive(Default)]
pub(crate) struct SiteClosures {
    /// How many defs were looked at.
    scanned: usize,
    /// The closures of user modules allocated since last looked at.
    pending: Vec<DefId>,
}

/// Variable `b`'s class is merged into `a`'s: what the closures of `b`'s site throw is `a`'s too
/// (`VarSets::union`).
pub(super) fn merge_closures(closures: &mut [(Vec<TyId>, Vec<u32>)], a: u32, b: u32) {
    let (ms, es) = std::mem::take(&mut closures[b as usize]);
    let (into, edges) = &mut closures[a as usize];
    for m in ms {
        if !into.contains(&m) {
            into.push(m);
        }
    }
    edges.extend(es);
}

impl Ctx<'_> {
    /// `t` (an error type) with each variable in it replaced by its set as known now, which the
    /// check then assumes it has (a `catch` variable's type).
    pub(crate) fn assume_vars_known(&mut self, t: Option<TyId>) -> Option<TyId> {
        let t = t?;
        if !self.mentions_err_var(t) {
            return Some(t);
        }
        let mut keep = vec![];
        for m in self.error_members(t) {
            let Some(v) = self.err_var(m) else {
                keep.push(m);
                continue;
            };
            let known = self.var_known(v);
            let set = self.error_union(&known).unwrap_or(self.ty.never);
            self.fn_sites.var_sets.assumed.push((v, set));
            for k in known {
                if !keep.contains(&k) {
                    keep.push(k);
                }
            }
        }
        self.error_union(&keep)
    }

    /// The members of variable `v`'s set from the constraints recorded so far (a fixpoint over
    /// the variables it reaches). Costs what those variables hold: what the closures throw is
    /// kept with each variable as they are found ([`Ctx::closures_now`]).
    fn var_known(&mut self, v: u32) -> Vec<TyId> {
        self.closures_now();
        let mut sets: FxHashMap<u32, Vec<TyId>> = FxHashMap::default();
        let mut edges: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        let root = self.fn_sites.var_sets.find(v);
        let mut work = vec![root];
        while let Some(r) = work.pop() {
            if sets.contains_key(&r) {
                continue;
            }
            let vs = &mut self.fn_sites.var_sets;
            let mut ms = vs.members.get(r as usize).cloned().unwrap_or_default();
            let mut out: Vec<u32> = vs.edges.get(r as usize).cloned().unwrap_or_default();
            if let Some((cms, ces)) = vs.closures.get(r as usize) {
                for m in cms {
                    if !ms.contains(m) {
                        ms.push(*m);
                    }
                }
                out.extend(ces.iter().copied());
            }
            let out: Vec<u32> = (out.into_iter())
                .map(|w| self.fn_sites.var_sets.find(w))
                .filter(|w| *w != r)
                .collect();
            work.extend(out.iter().copied());
            sets.insert(r, ms);
            edges.insert(r, out);
        }
        loop {
            let mut changed = false;
            for (r, out) in &edges {
                for e in out {
                    let add: Vec<TyId> = sets.get(e).cloned().unwrap_or_default();
                    let set = sets.get_mut(r).expect("ICE: a reached variable");
                    for m in add {
                        if !set.contains(&m) {
                            set.push(m);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        sets.remove(&root).unwrap_or_default()
    }

    /// Records each closure created for a site's type allocated since last called, whose body
    /// is checked: what it throws beyond its error type (as `Ctx::throw_into_vars` counts it)
    /// goes with the variable it throws into (`VarSets::closures`). A closure is looked at
    /// once (a later call through a function it calls may find more, which the solved set
    /// then holds: the program is checked again).
    fn closures_now(&mut self) {
        let n = self.info.len();
        let from = self.fn_sites.site_closures.scanned;
        for i in from..n {
            let d = DefId(i as u32);
            let Some(f) = self.try_fn(d) else {
                continue;
            };
            let user = self.scopes.get(f.module).is_some_and(|m| !m.is_std);
            if f.kind == FnKind::Closure && user {
                self.fn_sites.site_closures.pending.push(d);
            }
        }
        self.fn_sites.site_closures.scanned = n;
        let pending = std::mem::take(&mut self.fn_sites.site_closures.pending);
        let mut still = vec![];
        for d in pending {
            let Some(f) = self.try_fn(d) else {
                continue;
            };
            if f.state != BodyState::Done {
                still.push(d);
                continue;
            }
            let Some(decl) = f.declared_throws.and_then(|t| t.ty) else {
                continue;
            };
            let ms = self.error_members(decl);
            let Some(target) = ms.iter().find_map(|m| self.err_var(*m)) else {
                continue;
            };
            let srcs = self.fn_info(d).throw_srcs.clone();
            let own = crate::throws::srcs_now(self, &srcs);
            let errs: Vec<TyId> = (own.map(|t| self.error_members(t)).unwrap_or_default())
                .into_iter()
                .filter(|m| !ms.contains(m))
                .collect();
            let r = self.fn_sites.var_sets.find(target) as usize;
            for m in errs {
                let w = self.err_var(m);
                let (cms, ces) = &mut self.fn_sites.var_sets.closures[r];
                match w {
                    Some(w) => ces.push(w),
                    None if !cms.contains(&m) => cms.push(m),
                    None => {}
                }
            }
        }
        self.fn_sites.site_closures.pending = still;
    }

    /// A value of type `t` is used (a rejection reason in a `.catch` callback, a settled
    /// result's `reason`): when `t` is error-set variables (or holds them as members), the
    /// check, which takes each as written, nothing, is right only while that is its set. A
    /// variable nothing is known to be stored into yet is assumed empty, as a `catch` assumes;
    /// one with a known set makes the program checked again with the solved sets, where the
    /// value has its type.
    pub(crate) fn value_typed_by_vars(&mut self, t: TyId) {
        if !self.fn_sites.use_vars || self.fn_sites.vars.is_empty() || !self.mentions_err_var(t) {
            return;
        }
        let t = self.ty.core(t).unwrap_or(t);
        // The `reject` of a promise whose rejection type is a variable, used as a value (passed
        // on, stored): what it is called with there is not counted, so the program is checked
        // again with the solved sets, where `reject` takes the promise's error type. A value
        // that is called right away is not one ([`Ctx::reject_called`]).
        if self.is_reject_fn(t) {
            self.fn_sites.reject_values += 1;
            return;
        }
        let members = match self.union_members(t) {
            Some(ms) => ms,
            None => vec![t],
        };
        let vars: Vec<u32> = members.iter().filter_map(|m| self.err_var(*m)).collect();
        if vars.is_empty() || self.only_trap_vars(t) {
            return;
        }
        for v in vars {
            if self.var_known(v).is_empty() {
                self.fn_sites.var_sets.assumed.push((v, self.ty.never));
            } else {
                self.fn_sites.var_sets.unsupported = true;
            }
        }
    }

    /// The rejection type `e` of a promise type was shown (in a diagnostic) as written, its
    /// variables as nothing, where a check without variables shows their sets: what is shown
    /// is right only while their sets are empty, which the check then assumes
    /// ([`Ctx::assume_shown_empty`]).
    pub(crate) fn shown_as_nothing(&self, e: TyId) {
        if !self.fn_sites.use_vars || e == self.ty.never {
            return;
        }
        let mut shown = self.fn_sites.shown.borrow_mut();
        let ms = match self.union_def(e).and_then(|(d, _)| self.enum_info(d)) {
            Some(u) => (u.variants.iter())
                .filter_map(|v| v.payload.first().copied())
                .collect(),
            None => vec![e],
        };
        shown.extend(ms.into_iter().filter_map(|m| self.err_var(m)));
    }

    /// `f`'s result, where what it shows is named again once the variables are solved (a union
    /// def's name): it assumes nothing ([`Ctx::shown_as_nothing`]).
    pub(crate) fn unshown<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let before = std::mem::take(self.fn_sites.shown.get_mut());
        let r = f(self);
        *self.fn_sites.shown.get_mut() = before;
        r
    }

    /// The variables shown as nothing ([`Ctx::shown_as_nothing`]) are assumed empty: when one is
    /// not, the program is checked again with the solved sets, where its type shows its set.
    pub(crate) fn assume_shown_empty(&mut self) {
        let never = self.ty.never;
        let shown = std::mem::take(self.fn_sites.shown.get_mut());
        (self.fn_sites.var_sets.assumed).extend(shown.into_iter().map(|v| (v, never)));
    }

    /// Is `t` the type of the `reject` of a promise whose rejection type is a variable?
    fn is_reject_fn(&self, t: TyId) -> bool {
        let TyKind::FnPtr { params, .. } = self.ty.kind(t) else {
            return false;
        };
        (params.iter()).any(|p| self.fn_sites.rejects.iter().any(|(r, _)| r == p))
    }

    /// A value of type `t` is called: a `reject` checked as a value just before is called, not
    /// passed on ([`Ctx::value_typed_by_vars`]).
    pub(crate) fn reject_called(&mut self, t: TyId) {
        if self.fn_sites.reject_values > 0 && self.is_reject_fn(t) {
            self.fn_sites.reject_values -= 1;
        }
    }
}
