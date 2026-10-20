//! Error-set variables of function types written without `throws` (`fn_types`, docs/reference/
//! errors.md "Dynamic calls"; hir_encodings.md "Inferred error types of function types").
//!
//! Each site's function type is interned once, during collection, with a variable as its error
//! type (`TyOp::ErrSite`), so signatures, fields, interface members and the derived tables never
//! change. While bodies are checked, sema records constraints only:
//! - a closure created for a site's type throws into that site (its own errors ⊆ the variable);
//! - a value of one site's type stored as another's: the two are one set (unified);
//! - a function value with a known error type stored as a site's: the site's set is that one;
//! - a call through a site's value throws the variable, so the error types of the functions
//!   calling through it contain it, and a closure stored into another site carries it on.
//!
//! After inference the sets are solved (a fixpoint over the variables), and every type is
//! replaced by its solved form before lowering (`crate::readonly::erase`). The constructs that
//! need a solved set while bodies are checked (a `catch` variable's type) assume the variables
//! they meet are empty; when one is not, or a function value's error type differs from the
//! site's set it was stored as, the program is checked again with the solved sets written in,
//! as before (`FnSites::fallback`).

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind, TyOp};

/// The constraints on the variables of one check.
#[derive(Default)]
pub(crate) struct VarSets {
    /// Union-find parent of each variable (by `TyOp::ErrSite` number).
    parent: Vec<u32>,
    /// The error types stored into each variable (its representative's entry counts).
    members: Vec<Vec<TyId>>,
    /// Variables each one contains (`i ⊇ j`).
    edges: Vec<Vec<u32>>,
    /// Variables a check assumed empty (a `catch` variable's type).
    assumed_empty: Vec<u32>,
    /// A function value of a known error type stored as a site's: the set must be that type.
    exact: Vec<(u32, TyId)>,
    /// `t` with every variable replaced by `never` (the type as written).
    plain: FxHashMap<TyId, TyId>,
    /// Memo of `mentions_err_var`.
    mentions: FxHashMap<TyId, bool>,
    /// Something the variables can't express was met: check again with the solved sets.
    pub unsupported: bool,
}

impl VarSets {
    fn ensure(&mut self, v: u32) {
        while self.parent.len() <= v as usize {
            let n = self.parent.len() as u32;
            self.parent.push(n);
            self.members.push(vec![]);
            self.edges.push(vec![]);
        }
    }

    fn find(&mut self, v: u32) -> u32 {
        self.ensure(v);
        let mut r = v;
        while self.parent[r as usize] != r {
            r = self.parent[r as usize];
        }
        let mut c = v;
        while self.parent[c as usize] != r {
            let n = self.parent[c as usize];
            self.parent[c as usize] = r;
            c = n;
        }
        r
    }

    fn union(&mut self, a: u32, b: u32) {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return;
        }
        self.parent[b as usize] = a;
        let ms = std::mem::take(&mut self.members[b as usize]);
        self.members[a as usize].extend(ms);
        let es = std::mem::take(&mut self.edges[b as usize]);
        self.edges[a as usize].extend(es);
    }
}

impl Ctx<'_> {
    /// The variable `t` is (`TyOp::ErrSite`), if it is one.
    pub(crate) fn err_var(&self, t: TyId) -> Option<u32> {
        match self.ty.kind(t) {
            TyKind::Op(TyOp::ErrSite(n), _) => Some(*n),
            _ => None,
        }
    }

    /// Does `t` mention an error-set variable (also as a member of a union in it)?
    pub(crate) fn mentions_err_var(&mut self, t: TyId) -> bool {
        if self.fn_sites.vars.is_empty() {
            return false;
        }
        if let Some(&m) = self.fn_sites.var_sets.mentions.get(&t) {
            return m;
        }
        // Cycles (recursive types) count as not mentioning one while they are visited.
        self.fn_sites.var_sets.mentions.insert(t, false);
        let m = match self.ty.kind(t).clone() {
            TyKind::Op(TyOp::ErrSite(_), _) => true,
            k => {
                let mut parts = crate::types::children(&k);
                if self.union_def(t).is_some() {
                    parts.extend(self.union_members(t).unwrap_or_default());
                }
                parts.into_iter().any(|p| self.mentions_err_var(p))
            }
        };
        self.fn_sites.var_sets.mentions.insert(t, m);
        m
    }

    /// `t` with every variable replaced by `never`: the type as written.
    pub(crate) fn plain_type(&mut self, t: TyId) -> TyId {
        if let Some(&p) = self.fn_sites.var_sets.plain.get(&t) {
            return p;
        }
        let p = if self.err_var(t).is_some() {
            self.ty.never
        } else if !self.mentions_err_var(t) {
            t
        } else if let Some(ms) = self.union_members(t) {
            let ms: Vec<TyId> = ms.into_iter().map(|m| self.plain_type(m)).collect();
            self.error_union(&ms).unwrap_or(self.ty.never)
        } else {
            let k = self.ty.kind(t).clone();
            let k = crate::types::map_children(&k, &mut |x| self.plain_type(x));
            self.ty.intern(k)
        };
        self.fn_sites.var_sets.plain.insert(t, p);
        p
    }

    /// A value of type `from` used as a `to`: equal but for their variables. Records that the
    /// variables they hold in the same place are one set (or that a variable is the error
    /// type a function value has); false when they differ otherwise.
    pub(crate) fn unify_err_vars(&mut self, from: TyId, to: TyId) -> bool {
        if from == to || !(self.mentions_err_var(from) || self.mentions_err_var(to)) {
            return false;
        }
        if self.plain_type(from) != self.plain_type(to) {
            return false;
        }
        self.unify_parts(from, to, 0);
        true
    }

    fn unify_parts(&mut self, a: TyId, b: TyId, depth: u32) {
        if a == b || depth > 32 {
            return;
        }
        match (self.err_var(a), self.err_var(b)) {
            (Some(x), Some(y)) => return self.fn_sites.var_sets.union(x, y),
            (Some(x), None) | (None, Some(x)) => {
                let other = if self.err_var(a).is_some() { b } else { a };
                self.fn_sites.var_sets.ensure(x);
                self.fn_sites.var_sets.exact.push((x, other));
                return;
            }
            (None, None) => {}
        }
        // Error types are compared as sets: a union holding a variable is matched member-wise
        // in `plain_type`'s order only when it holds just one.
        if self.union_members(a).is_some() || self.union_members(b).is_some() {
            let va: Vec<u32> = self.vars_of(a);
            let vb: Vec<u32> = self.vars_of(b);
            if va.len() + vb.len() > 0 {
                self.fn_sites.var_sets.unsupported = true;
            }
            return;
        }
        let ka = crate::types::children(self.ty.kind(a));
        let kb = crate::types::children(self.ty.kind(b));
        if ka.len() != kb.len() {
            return;
        }
        for (x, y) in ka.into_iter().zip(kb) {
            self.unify_parts(x, y, depth + 1);
        }
    }

    /// The variables among the members of error type `t`.
    fn vars_of(&mut self, t: TyId) -> Vec<u32> {
        let ms = self.error_members(t);
        ms.into_iter().filter_map(|m| self.err_var(m)).collect()
    }

    /// Closure `d`, created for a site's type, throws `own`: add what its error type `decl`
    /// does not hold to the variables in `decl`. False when `decl` holds none.
    pub(crate) fn throw_into_vars(&mut self, decl: Option<TyId>, own: Option<TyId>) -> bool {
        let Some(decl) = decl else {
            return false;
        };
        let dv = self.vars_of(decl);
        let Some(&first) = dv.first() else {
            return false;
        };
        let allowed = self.error_members(decl);
        for m in own.map(|t| self.error_members(t)).unwrap_or_default() {
            if allowed.contains(&m) {
                continue;
            }
            let r = self.fn_sites.var_sets.find(first);
            match self.err_var(m) {
                Some(v) => {
                    let v = self.fn_sites.var_sets.find(v);
                    if v != r && !self.fn_sites.var_sets.edges[r as usize].contains(&v) {
                        self.fn_sites.var_sets.edges[r as usize].push(v);
                    }
                }
                None => {
                    if !self.is_error_class_ty(m) {
                        // Only `Error` classes are inferred (`fn_types::seed_fn_sites`).
                        return false;
                    }
                    if !self.fn_sites.var_sets.members[r as usize].contains(&m) {
                        self.fn_sites.var_sets.members[r as usize].push(m);
                    }
                }
            }
        }
        true
    }

    /// `t` without its variables, which a check assumes empty (a `catch` variable's type).
    pub(crate) fn assume_vars_empty(&mut self, t: Option<TyId>) -> Option<TyId> {
        let t = t?;
        if !self.mentions_err_var(t) {
            return Some(t);
        }
        let mut keep = vec![];
        for m in self.error_members(t) {
            match self.err_var(m) {
                Some(v) => self.fn_sites.var_sets.assumed_empty.push(v),
                None => keep.push(m),
            }
        }
        self.error_union(&keep)
    }

    /// Solve the variables (a fixpoint over `i ⊇ j`), fill `FnSites::solved`, and check what
    /// the check assumed. False when the program must be checked again with the solved sets.
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
        let mut ok = !self.fn_sites.var_sets.unsupported;
        for v in 0..n {
            let r = reps[v] as usize;
            let s = self.error_union(&sets[r]).unwrap_or(self.ty.never);
            let var = self.ty.intern(TyKind::Op(TyOp::ErrSite(v as u32), vec![]));
            self.fn_sites.solved.insert(var, s);
        }
        for v in std::mem::take(&mut self.fn_sites.var_sets.assumed_empty) {
            ok &= sets[reps[v as usize] as usize].is_empty();
        }
        for (v, t) in std::mem::take(&mut self.fn_sites.var_sets.exact) {
            let want = self.solved_type(t);
            let have = self.error_union(&sets[reps[v as usize] as usize]);
            ok &= self.canon_error(Some(want)) == have;
        }
        ok
    }

    /// `t` with its variables replaced by their solved types.
    pub(crate) fn solved_type(&mut self, t: TyId) -> TyId {
        if let Some(&s) = self.fn_sites.solved.get(&t) {
            return s;
        }
        if !self.mentions_err_var(t) {
            return t;
        }
        let s = if let Some(ms) = self.union_members(t) {
            let ms: Vec<TyId> = ms.into_iter().map(|m| self.solved_type(m)).collect();
            self.error_union(&ms).unwrap_or(self.ty.never)
        } else {
            let k = self.ty.kind(t).clone();
            let k = crate::types::map_children(&k, &mut |x| self.solved_type(x));
            self.ty.intern(k)
        };
        self.fn_sites.solved.insert(t, s);
        s
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

    /// Is `t` the prelude's `Error` or a class extending it?
    fn is_error_class_ty(&mut self, t: TyId) -> bool {
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

    /// The solved sets as seeds for checking the program again (`FnSites::fallback`).
    pub(crate) fn var_seeds(&mut self) {
        self.fn_sites.fallback();
        let spans = self.fn_sites.vars.clone();
        for (v, span) in spans.into_iter().enumerate() {
            let var = self.ty.intern(TyKind::Op(TyOp::ErrSite(v as u32), vec![]));
            let s = self.fn_sites.solved.get(&var).copied();
            let Some(s) = s.filter(|s| *s != self.ty.never) else {
                continue;
            };
            let ms: Vec<_> = self.error_members(s);
            let ps: Vec<_> = ms.into_iter().filter_map(|m| self.port_type(m)).collect();
            self.add_seed(span, ps);
        }
    }
}
