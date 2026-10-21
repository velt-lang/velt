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
//! - types compared where values pass both ways (a method's signature and the one it
//!   implements or overrides, operands of `===`) are one type but for their variables, which
//!   then are one set ([`Ctx::same_ty`]).
//!
//! After inference the sets are solved (a fixpoint over the variables), the functions' error
//! types are replaced by their solved forms, and every type is replaced by its solved form
//! before lowering (`crate::readonly::erase`). The constructs that need a solved set while bodies are checked (a
//! `catch` variable's type, a `finally` block of a generator, a value converted as its type
//! written without variables) assume the variables they meet are empty; when one is not, or a
//! function value's error type differs from the site's set it was stored as, the program is
//! checked again with the solved sets written in, as before (`FnSites::fallback`). When the
//! check reports errors and two types that differ in more than their variables were compared
//! where they must be one type, or a variable was outside a bound, the program is checked
//! again without variables too (`FnSites::doubted`): its errors are then exactly those of a
//! check without them. A conversion that fails with the variables and as written fails
//! without them too, and diagnostics show a type with variables as written (a variable as
//! nothing), so neither raises that doubt.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::anon::ShapeField;
use crate::hir::{AdtKind, DefId, TyId, TyKind, TyOp};

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
    /// The earliest type id each plain or solved type stands for: the id it would have in a
    /// check without variables, as far as the order goes (`Ctx::solved_union`).
    first: FxHashMap<TyId, u32>,
    /// Memo of `mentions_err_var`.
    mentions: FxHashMap<TyId, bool>,
    /// Something the variables can't express was met: check again with the solved sets.
    pub unsupported: bool,
}

impl VarSets {
    /// `p` is the plain or solved form of `t`.
    fn note_first(&mut self, p: TyId, t: TyId) {
        let k = self.order_key(p).min(self.order_key(t));
        self.first.insert(p, k);
    }

    /// Where `t` sorts among a union's members: the id of the type it is without variables
    /// when that is known, else its own (`Ctx::solved_union`).
    pub(crate) fn order_key(&self, t: TyId) -> u32 {
        self.first.get(&t).copied().unwrap_or(t.0)
    }

    /// `t`, a site's type with a variable, is `plain` as written.
    pub(super) fn note_written(&mut self, t: TyId, plain: TyId) {
        self.first.insert(t, plain.0);
    }

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

    /// Is `t` an error-set variable?
    pub(crate) fn is_err_var(&self, t: TyId) -> bool {
        self.err_var(t).is_some()
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
                // The fields of an anonymous object type are part of the type.
                if let Some((d, _)) = self.anon_of(t) {
                    parts.extend(self.anon_field_tys(d));
                }
                parts.into_iter().any(|p| self.mentions_err_var(p))
            }
        };
        self.fn_sites.var_sets.mentions.insert(t, m);
        m
    }

    /// Does an anonymous object type in `t` have a field type with a variable? The
    /// comparisons don't look into such fields (`plain_type`, `unify_parts`).
    pub(crate) fn anon_fields_hold_vars(&mut self, t: TyId) -> bool {
        if !self.fn_sites.use_vars || self.fn_sites.vars.is_empty() {
            return false;
        }
        let mut seen = rustc_hash::FxHashSet::default();
        let mut stack = vec![t];
        while let Some(t) = stack.pop() {
            if !seen.insert(t) || seen.len() > 4096 {
                continue;
            }
            let k = self.ty.kind(t).clone();
            stack.extend(crate::types::children(&k));
            if let TyKind::Adt(d, _) = k {
                if self
                    .adt(d)
                    .is_some_and(|a| a.kind == crate::hir::AdtKind::Anon)
                {
                    let fs = self.anon_field_tys(d);
                    if fs.iter().any(|f| self.mentions_err_var(*f)) {
                        return true;
                    }
                    stack.extend(fs);
                }
            }
        }
        false
    }

    /// Could a mismatch between `a` and `b` come from a variable in a field of an anonymous
    /// object type, which the comparisons don't see yet? Only when one of them has such a field
    /// and the two have one shape but for their error types: an object type converted to an
    /// interface it does not implement, or to an object type with other fields, is a mismatch
    /// without variables too.
    pub(crate) fn anon_vars_may_mismatch(&mut self, a: TyId, b: TyId) -> bool {
        (self.anon_fields_hold_vars(a) || self.anon_fields_hold_vars(b))
            && self.same_but_errors(a, b, 0)
    }

    /// Are `a` and `b` one type but for their error types (what function types throw, what
    /// promises reject with), field by field for anonymous object types? True where that is not
    /// known (deferred types, unions, interface values a type may implement).
    fn same_but_errors(&mut self, a: TyId, b: TyId, depth: u32) -> bool {
        if a == b || depth > 32 {
            return true;
        }
        let same = |cx: &mut Self, x: &[TyId], y: &[TyId]| {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|(p, q)| cx.same_but_errors(*p, *q, depth + 1))
        };
        if self.union_def(a).is_some() || self.union_def(b).is_some() {
            return true;
        }
        let anon = |cx: &Self, d: crate::hir::DefId| {
            cx.adt(d)
                .is_some_and(|a| a.kind == crate::hir::AdtKind::Anon)
        };
        match (self.ty.kind(a).clone(), self.ty.kind(b).clone()) {
            (TyKind::Op(..), _) | (_, TyKind::Op(..)) | (TyKind::Error, _) | (_, TyKind::Error) => {
                true
            }
            (
                TyKind::FnPtr {
                    params: p, ret: r, ..
                },
                TyKind::FnPtr {
                    params: q, ret: s, ..
                },
            ) => same(self, &p, &q) && same(self, &[r], &[s]),
            (TyKind::Promise(x, _), TyKind::Promise(y, _))
            | (TyKind::Array(x), TyKind::Array(y))
            | (TyKind::Shared(x), TyKind::Shared(y)) => same(self, &[x], &[y]),
            (TyKind::Option(x, n), TyKind::Option(y, m)) => n == m && same(self, &[x], &[y]),
            (TyKind::Map(x, y), TyKind::Map(z, w))
            | (TyKind::Result(x, y), TyKind::Result(z, w)) => same(self, &[x, y], &[z, w]),
            (TyKind::Tuple(xs), TyKind::Tuple(ys)) => same(self, &xs, &ys),
            (TyKind::Adt(d, xs), TyKind::Adt(e, ys)) if d == e => same(self, &xs, &ys),
            (TyKind::Adt(d, xs), TyKind::Adt(e, ys)) if anon(self, d) && anon(self, e) => {
                let fields = |cx: &Self, d| {
                    cx.adt(d)
                        .map(|a| {
                            a.fields
                                .iter()
                                .map(|f| (f.name.clone(), f.optional, f.ty))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                };
                let (fa, fb) = (fields(self, d), fields(self, e));
                if fa.len() != fb.len() || fa.iter().zip(&fb).any(|(x, y)| x.0 != y.0 || x.1 != y.1)
                {
                    return false;
                }
                let ta: Vec<TyId> = fa.iter().map(|f| self.subst(f.2, &xs)).collect();
                let tb: Vec<TyId> = fb.iter().map(|f| self.subst(f.2, &ys)).collect();
                same(self, &ta, &tb)
            }
            // A class instance converted to an object type is copied field by field.
            (TyKind::Adt(..), TyKind::Adt(..)) => true,
            // An object type is converted to an interface only by an implementation, which an
            // anonymous object type does not have.
            (TyKind::Dyn(..), TyKind::Adt(d, _)) | (TyKind::Adt(d, _), TyKind::Dyn(..)) => {
                !anon(self, d)
            }
            (TyKind::Dyn(..), _) | (_, TyKind::Dyn(..)) => true,
            _ => false,
        }
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
        } else if let Some(orig) = self.union_members(t) {
            let ms: Vec<TyId> = orig.iter().map(|m| self.plain_type(*m)).collect();
            self.solved_union(&orig, &ms)
        } else if let Some((d, args)) = self.anon_of(t) {
            self.plain_anon(d, &args)
        } else {
            let k = self.ty.kind(t).clone();
            let k = crate::types::map_children(&k, &mut |x| self.plain_type(x));
            self.ty.intern(k)
        };
        self.fn_sites.var_sets.plain.insert(t, p);
        self.fn_sites.var_sets.note_first(p, t);
        p
    }

    /// `t`'s anonymous object def and type arguments, when it is an anonymous object type.
    fn anon_of(&self, t: TyId) -> Option<(DefId, Vec<TyId>)> {
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if self.adt(*d).is_some_and(|a| a.kind == AdtKind::Anon) => {
                Some((*d, args.clone()))
            }
            _ => None,
        }
    }

    /// Anonymous object type `d<args>` as written: the object type of its fields' plain
    /// types (the def a literal of that shape has).
    fn plain_anon(&mut self, d: DefId, args: &[TyId]) -> TyId {
        let Some(a) = self.adt(d) else {
            return self.ty.intern(TyKind::Adt(d, args.to_vec()));
        };
        let module = a.module;
        let fields: Vec<ShapeField> = a.fields.iter().map(crate::anon::shape_field).collect();
        let fields: Vec<ShapeField> = (fields.into_iter())
            .map(|f| ShapeField {
                ty: self.plain_type(f.ty),
                ..f
            })
            .collect();
        let (d2, params) = self.anon_def_with(&fields, module);
        let args: Vec<TyId> = args.iter().map(|a| self.plain_type(*a)).collect();
        let args2: Vec<TyId> = params.iter().map(|p| self.ty.subst(*p, &args)).collect();
        self.ty.intern(TyKind::Adt(d2, args2))
    }

    /// The anonymous object defs whose fields hold variables, each with the def of its solved
    /// shape, which `crate::readonly::erase` puts in its place: a def of those field types
    /// made while checking, or else the first def of that solved shape. Values of two such
    /// types of one shape are stored as each other's unconverted (they were unified).
    pub(crate) fn solved_anon_twins(&mut self) -> Vec<(DefId, DefId)> {
        let mut out = vec![];
        let mut first: FxHashMap<Vec<ShapeField>, DefId> = FxHashMap::default();
        for i in 0..self.info.len() {
            let d = DefId(i as u32);
            let Some(a) = self.adt(d).filter(|a| a.kind == AdtKind::Anon) else {
                continue;
            };
            let fields: Vec<ShapeField> = a.fields.iter().map(crate::anon::shape_field).collect();
            let tys: Vec<TyId> = a.fields.iter().map(|f| f.ty).collect();
            if !tys.into_iter().any(|t| self.mentions_err_var(t)) || self.reaches_itself(d) {
                continue;
            }
            let solved: Vec<ShapeField> = (fields.into_iter())
                .map(|f| ShapeField {
                    ty: self.solved_type(f.ty),
                    ..f
                })
                .collect();
            let twin = match self.anon.get(&solved) {
                Some(&t) => t,
                None => *first.entry(solved).or_insert(d),
            };
            if twin != d {
                out.push((d, twin));
            }
        }
        out
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

    /// Are `a` and `b` one type, or one but for their error-set variables (which become one
    /// set, [`unify_err_vars`](Self::unify_err_vars))? For types a value passes between both
    /// ways (a method's signature and the one it implements or overrides, operands compared).
    pub(crate) fn same_ty(&mut self, a: TyId, b: TyId) -> bool {
        if a == b || !self.fn_sites.use_vars {
            return a == b;
        }
        let same = self.unify_err_vars(a, b);
        if !same && (self.mentions_err_var(a) || self.mentions_err_var(b)) {
            self.fn_sites.set_doubt();
        }
        same
    }

    /// Are `a` and `b` pairwise equal but for their variables? Then they are unified.
    pub(crate) fn unify_err_var_lists(&mut self, a: &[TyId], b: &[TyId]) -> bool {
        if !self.fn_sites.use_vars || a.len() != b.len() {
            return false;
        }
        let pairs: Vec<(TyId, TyId)> = a.iter().copied().zip(b.iter().copied()).collect();
        let mut vars = false;
        for &(x, y) in &pairs {
            vars |= x != y && (self.mentions_err_var(x) || self.mentions_err_var(y));
            if self.plain_type(x) != self.plain_type(y) {
                return false;
            }
        }
        if !vars {
            return false;
        }
        for (x, y) in pairs {
            self.unify_parts(x, y, 0);
        }
        true
    }

    /// [`same_ty`](Self::same_ty) for each pair (all of them unified).
    pub(crate) fn same_tys(&mut self, a: &[TyId], b: &[TyId]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .fold(true, |ok, (x, y)| self.same_ty(*x, *y) && ok)
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
        // Anonymous object types of two defs: their fields, at their type arguments.
        if let (Some((d, xs)), Some((e, ys))) = (self.anon_of(a), self.anon_of(b)) {
            if d != e {
                let fa = self.anon_field_tys(d);
                let fb = self.anon_field_tys(e);
                if fa.len() == fb.len() {
                    for (x, y) in fa.into_iter().zip(fb) {
                        let x = self.ty.subst(x, &xs);
                        let y = self.ty.subst(y, &ys);
                        self.unify_parts(x, y, depth + 1);
                    }
                }
                return;
            }
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

    /// Assume every variable in `t`, at any depth, empty (a value converted as its type
    /// written without them).
    pub(crate) fn assume_empty_in(&mut self, t: TyId) {
        let mut seen = rustc_hash::FxHashSet::default();
        self.vars_deep(t, &mut seen);
    }

    fn vars_deep(&mut self, t: TyId, seen: &mut rustc_hash::FxHashSet<TyId>) {
        if !seen.insert(t) || !self.mentions_err_var(t) {
            return;
        }
        if let Some(v) = self.err_var(t) {
            self.fn_sites.var_sets.assumed_empty.push(v);
            return;
        }
        let mut parts = crate::types::children(self.ty.kind(t));
        if self.union_def(t).is_some() {
            parts.extend(self.union_members(t).unwrap_or_default());
        }
        for p in parts {
            self.vars_deep(p, seen);
        }
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
        let mut ok = !self.fn_sites.var_sets.unsupported;
        for (v, &r) in reps.iter().enumerate() {
            let s = self.error_union(&sets[r as usize]).unwrap_or(self.ty.never);
            let var = self.ty.intern(TyKind::Op(TyOp::ErrSite(v as u32), vec![]));
            self.fn_sites.solved.insert(var, s);
        }
        for (v, t) in std::mem::take(&mut self.fn_sites.var_sets.exact) {
            let want = self.solved_type(t);
            let have = self.error_union(&sets[reps[v as usize] as usize]);
            ok &= self.canon_error(Some(want)) == have;
        }
        ok
    }

    /// Are the variables a check assumed empty (a `catch` variable's type) empty?
    pub(crate) fn assumptions_hold(&mut self) -> bool {
        let never = self.ty.never;
        let assumed = std::mem::take(&mut self.fn_sites.var_sets.assumed_empty);
        assumed.into_iter().all(|v| {
            let var = self.ty.intern(TyKind::Op(TyOp::ErrSite(v), vec![]));
            self.fn_sites.solved.get(&var).is_none_or(|s| *s == never)
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
            self.ty.intern(k)
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
    fn solved_union(&mut self, orig: &[TyId], ms: &[TyId]) -> TyId {
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
        order.sort_by_key(|(k, _)| *k);
        let order: Vec<TyId> = order.into_iter().map(|(_, m)| m).collect();
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
