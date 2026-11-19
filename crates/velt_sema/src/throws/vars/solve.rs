//! The error variables solved ([`super`]): each variable's set from what was thrown into it,
//! whether the assumptions made while checking hold, and the solved sets written into types.

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind, TyOp};

impl Ctx<'_> {
    /// Solve the variables (a fixpoint over `i ⊇ j`), fill `FnSites::solved`, and check the
    /// function values stored as sites' types. False when the program must be checked again
    /// with the solved sets.
    pub(crate) fn solve_err_vars(&mut self) -> bool {
        let n = self.fn_sites.vars.len();
        self.fn_sites.var_sets.ensure(n.saturating_sub(1) as u32);
        let reps: Vec<u32> = (0..n as u32)
            .map(|v| self.fn_sites.var_sets.find(v))
            .collect();
        let mut sets: Vec<Vec<TyId>> = (0..n)
            .map(|v| self.fn_sites.var_sets.members[v].clone())
            .collect();
        loop {
            let mut changed = false;
            for r in 0..n {
                let edges = self.fn_sites.var_sets.edges[r].clone();
                for e in edges {
                    let e = reps[e as usize] as usize;
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
        let mut ok = !self.fn_sites.var_sets.unsupported && self.fn_sites.reject_values == 0;
        self.fn_sites.sets_empty = Some(reps.iter().all(|&r| sets[r as usize].is_empty()));
        for (v, &r) in reps.iter().enumerate() {
            let s = self.error_union(&sets[r as usize]).unwrap_or(self.ty.never);
            let var = self.ty.intern(TyKind::Op(TyOp::ErrSite(v as u32), vec![]));
            self.fn_sites.solved.insert(var, s);
        }
        for (v, t) in std::mem::take(&mut self.fn_sites.var_sets.exact) {
            let want = self.solved_type(t);
            let have = self.error_union(&sets[reps[v as usize] as usize]);
            if std::env::var_os("VELT_UNITS_GROW_DEBUG").is_some()
                && self.canon_error(Some(want)) != have
            {
                let site = self.fn_sites.vars.get(v as usize).copied();
                eprintln!(
                    "velt-units: exact var {v} (trap {}) at {site:?}: want {} have {}",
                    self.fn_sites.trap_vars.contains(&v),
                    self.display(want),
                    have.map_or("none".into(), |h| self.display(h))
                );
            }
            ok &= self.canon_error(Some(want)) == have;
        }
        ok
    }

    /// Do the variables have the sets a check assumed (empty, or what was known then)?
    pub(crate) fn assumptions_hold(&mut self) -> bool {
        let assumed = std::mem::take(&mut self.fn_sites.var_sets.assumed);
        assumed.into_iter().all(|(v, t)| {
            let var = self.ty.intern(TyKind::Op(TyOp::ErrSite(v), vec![]));
            let s = self.fn_sites.solved.get(&var).copied();
            let ok = s.unwrap_or(self.ty.never) == t;
            if !ok && std::env::var_os("VELT_UNITS_GROW_DEBUG").is_some() {
                let s = s.unwrap_or(self.ty.never);
                eprintln!(
                    "velt-units: assumed var {v} (trap {}, fixed {}) = {}, solved {}",
                    self.fn_sites.trap_vars.contains(&v),
                    self.fn_sites.fixed_traps.contains_key(&v),
                    self.display(t),
                    self.display(s)
                );
            }
            ok
        })
    }

    /// `t` with its variables replaced by their solved types.
    pub(crate) fn solved_type(&mut self, t: TyId) -> TyId {
        if let Some(&s) = self.fn_sites.solved.get(&t) {
            return s;
        }
        if !self.mentions_err_var(t) {
            return t;
        }
        let s = if let Some(orig) = self.union_members(t) {
            let ms: Vec<TyId> = orig.iter().map(|m| self.solved_type(*m)).collect();
            self.solved_union(&orig, &ms)
        } else {
            let k = self.ty.kind(t).clone();
            let k = crate::types::map_children(&k, &mut |x| self.solved_type(x));
            let out = self.ty.intern(k);
            self.ty.keep_readonly(t, out)
        };
        self.fn_sites.solved.insert(t, s);
        self.fn_sites.var_sets.note_first(s, t);
        s
    }

    /// The union of `ms`, the solved (or plain) forms of the members `orig` of a union with
    /// variables. A union's members are ordered by type id, and a solved member is interned
    /// later than the type it would be without variables: a new union def made here orders
    /// each member by the earlier of the two ids, the order it has without variables (and the
    /// order of its variants in VIR).
    pub(super) fn solved_union(&mut self, orig: &[TyId], ms: &[TyId]) -> TyId {
        let before = self.unions.len();
        let u = self.error_union(ms).unwrap_or(self.ty.never);
        if self.unions.len() == before {
            return u;
        }
        let Some((d, args)) = self.union_def(u) else {
            return u;
        };
        let plain = |cx: &Self, t: TyId| {
            cx.union_def(t).is_none()
                && !matches!(
                    cx.ty.kind(t),
                    TyKind::Option(..) | TyKind::Never | TyKind::Unit
                )
        };
        if !args.is_empty() || !ms.iter().all(|m| plain(self, *m)) {
            return u;
        }
        let mut order: Vec<(u32, TyId)> = vec![];
        for (&o, &m) in orig.iter().zip(ms) {
            let vs = &self.fn_sites.var_sets;
            let key = vs.order_key(m).min(vs.order_key(o));
            match order.iter_mut().find(|(_, x)| *x == m) {
                Some(e) => e.0 = e.0.min(key),
                None => order.push((key, m)),
            }
        }
        // The order `union_with` gives a union (`Ctx::canon_key` of the type each member stands
        // for), so the solved union reads as the one written.
        let mut keyed: Vec<(std::rc::Rc<str>, u32, TyId)> = (order.into_iter())
            .map(|(k, m)| (self.canon_key(TyId(k)), m.0, m))
            .collect();
        keyed.sort();
        let order: Vec<TyId> = keyed.into_iter().map(|k| k.2).collect();
        self.reorder_union_def(d, &order);
        u
    }

    /// Every interned type that mentions a variable, with its solved form (for
    /// `crate::readonly::erase`).
    pub(crate) fn solved_types(&mut self) -> Vec<(TyId, TyId)> {
        if self.fn_sites.vars.is_empty() {
            return vec![];
        }
        let n = self.ty.table.len();
        let mut out = vec![];
        for i in 0..n {
            let t = TyId(i as u32);
            if self.mentions_err_var(t) {
                let s = self.solved_type(t);
                out.push((t, s));
            }
        }
        out
    }

    /// The solved sets as seeds for checking the program again (`FnSites::fallback`).
    pub(crate) fn var_seeds(&mut self) {
        self.fn_sites.fallback();
        // Every site of a variable: a function type's result `Promise<T>` shares its function
        // type's (`fn_types`), and a check without variables has a set for each.
        let mut spans: Vec<(TyId, velt_common::Span)> = (self.fn_sites.var_of.iter())
            .map(|(s, v)| (*v, *s))
            .collect();
        spans.sort_by_key(|(v, s)| (v.0, s.file, s.lo, s.hi));
        let seeds = self.seed_sets();
        for (var, span) in spans {
            let Some(ms) = seeds.get(&var).filter(|ms| !ms.is_empty()).cloned() else {
                continue;
            };
            let ps: Vec<_> = ms.into_iter().filter_map(|m| self.port_type(m)).collect();
            self.add_seed(span, ps);
        }
    }
}
