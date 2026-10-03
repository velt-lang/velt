//! Error types as sets: a function's (or `try` block's) error type is the canonical union of
//! everything it can throw (`crate::unions`), so `A | B` and `B | A` are one type; `None` (or
//! `never`) is the empty set.

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

impl Ctx<'_> {
    /// The thrown types `t` stands for: its members when it is a union (also a non-canonical
    /// one produced by substituting a union into a generic union), nothing for `never`.
    pub(crate) fn error_members(&mut self, t: TyId) -> Vec<TyId> {
        let mut out = vec![];
        self.collect_error_members(t, &mut out, 0);
        out
    }

    fn collect_error_members(&mut self, t: TyId, out: &mut Vec<TyId>, depth: u32) {
        if t == self.ty.never || depth > 32 {
            return;
        }
        match self.union_members(t) {
            Some(ms) => {
                for m in ms {
                    self.collect_error_members(m, out, depth + 1);
                }
            }
            None if !out.contains(&t) => out.push(t),
            None => {}
        }
    }

    /// The canonical error type of a set of thrown types (`None`: nothing is thrown). `void`
    /// throws nothing: written as an error type it is reported where it is written (`throws
    /// void`, `Promise<T, void>`), and substituted for a type parameter it drops out.
    pub(crate) fn error_union(&mut self, members: &[TyId]) -> Option<TyId> {
        let mut flat = vec![];
        for &m in members {
            for x in self.error_members(m) {
                if x != self.ty.unit && !flat.contains(&x) {
                    flat.push(x);
                }
            }
        }
        if flat.is_empty() {
            return None;
        }
        Some(self.union_of(&flat, false, Span::DUMMY))
    }

    /// Canonical form of an optional error type (`Some(never)` → `None`).
    pub(crate) fn canon_error(&mut self, t: Option<TyId>) -> Option<TyId> {
        t.and_then(|t| self.error_union(&[t]))
    }

    /// `a ∪ b`.
    pub(crate) fn join_errors(&mut self, a: Option<TyId>, b: Option<TyId>) -> Option<TyId> {
        let ms: Vec<TyId> = a.into_iter().chain(b).collect();
        self.error_union(&ms)
    }

    /// The first member of `t` that `bound` does not allow (a member is allowed by an equal
    /// bound member, or by a base class of it: the error value is upcast).
    pub(crate) fn error_outside(&mut self, bound: Option<TyId>, t: Option<TyId>) -> Option<TyId> {
        let allowed = bound.map(|b| self.error_members(b)).unwrap_or_default();
        let found = t.map(|t| self.error_members(t)).unwrap_or_default();
        found
            .into_iter()
            .find(|m| !self.ty.is_bottom(*m) && !allowed.iter().any(|b| self.error_fits(*m, *b)))
    }

    /// Can an error of type `m` be stored as a `b` (equal, or `b` is a base class of `m`)?
    fn error_fits(&mut self, m: TyId, b: TyId) -> bool {
        if m == b || self.ty.is_bottom(b) {
            return true;
        }
        let mut cur = m;
        for _ in 0..64 {
            match self.base_of(cur) {
                Some(p) if p == b => return true,
                Some(p) => cur = p,
                None => return false,
            }
        }
        false
    }

    /// Does `t` mention a type parameter?
    pub(crate) fn mentions_params(&self, t: TyId) -> bool {
        let mut ps = vec![];
        crate::types::collect_params(&self.ty, t, &mut ps);
        !ps.is_empty() || matches!(self.ty.kind(t), TyKind::Param(_))
    }
}
