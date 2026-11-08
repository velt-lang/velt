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

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::defs::{BodyState, FnKind};
use crate::hir::{DefId, TyId};

/// The closures created for a site's type (their error type holds its variable), found so far.
#[derive(Default)]
pub(crate) struct SiteClosures {
    /// How many defs were looked at.
    scanned: usize,
    /// The closures of user modules whose bodies were not checked yet when last looked at.
    pending: Vec<DefId>,
    /// Each checked one created for a site's type: the variable it throws into and what it
    /// threw, beyond its error type, when it was first looked at once checked.
    known: Vec<(u32, Vec<TyId>)>,
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
    /// the variables it reaches).
    fn var_known(&mut self, v: u32) -> Vec<TyId> {
        let own = self.closures_now();
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
            for (target, errs) in &own {
                if self.fn_sites.var_sets.find(*target) != r {
                    continue;
                }
                for &m in errs {
                    match self.err_var(m) {
                        Some(w) => out.push(w),
                        None if !ms.contains(&m) => ms.push(m),
                        None => {}
                    }
                }
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

    /// Each checked closure created for a site's type: the variable it throws into and what it
    /// throws beyond its error type (as `Ctx::throw_into_vars` counts it), found once its body
    /// is checked (a later call through a function it calls may find more, which the solved
    /// set then holds: the program is checked again).
    fn closures_now(&mut self) -> Vec<(u32, Vec<TyId>)> {
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
            self.fn_sites.site_closures.known.push((target, errs));
        }
        self.fn_sites.site_closures.pending = still;
        self.fn_sites.site_closures.known.clone()
    }
}
