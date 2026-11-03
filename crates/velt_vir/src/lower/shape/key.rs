//! The shape of a type argument (design note section 1) and the checks that keep it exact.
//!
//! Inside a shared instance, every question about a placeholder that lowering answers from
//! layout or ownership is answered for the representative (`Cx::ph_rep` at the top of those
//! queries), and the counted-types sets hold each type built from a placeholder where they
//! hold the same type built from the representative (`mirror`). So the key is every answer
//! such code may depend on for the type itself and for the derived types the shared helpers
//! use (`T[]`); `verify` checks, at the end of the pass, that every member of each shape gives
//! the representative's answers for every type built from its placeholder.

use rustc_hash::FxHashSet;
use velt_sema::hir::{TyId, TyKind};

use super::super::{ice, Cx};
use crate::vir::Ty;

/// A type's layout as shared code sees it: aggregates by size and alignment.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Lay {
    Scalar(Ty),
    Agg(u32, u32),
}

/// The shape key of a type argument.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::lower) struct Key {
    lay: Lay,
    /// `ShareKind` as a number, and the bits shared code may branch on.
    share: u8,
    bits: u32,
    /// The same answers for `T[]`.
    array: u32,
}

impl Cx<'_> {
    /// The representative of placeholder `t`, or `t` itself.
    pub(in crate::lower) fn ph_rep(&self, t: TyId) -> TyId {
        match self.types.kind(t) {
            TyKind::Param(n) if *n >= super::SHAPE_BASE => {
                self.shapes.ph[(*n - super::SHAPE_BASE) as usize].rep
            }
            _ => t,
        }
    }

    /// The shape key of concrete `t`, or `None` when `t` is not shared in this step.
    pub(super) fn shape_key(&mut self, t: TyId) -> Option<Key> {
        let shareable = match self.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str => true,
            TyKind::Adt(..) | TyKind::Array(_) | TyKind::Option(..) | TyKind::Tuple(_) => true,
            _ => false,
        };
        if !shareable || self.mentions_any_param(t) || self.is_dynamic(t) {
            return None;
        }
        let lay = match self.ty(t) {
            Ty::Agg(a) => {
                let l = &self.aggs[a.0 as usize];
                Lay::Agg(l.size, l.align)
            }
            Ty::Unit | Ty::V128 => return None,
            s => Lay::Scalar(s),
        };
        let share = self.share_kind(t) as u8;
        let bits = self.bits(t);
        let arr = self.intern(TyKind::Array(t));
        let array = self.bits(arr) | u32::from(self.array_view(arr)) << 16;
        Some(Key {
            lay,
            share,
            bits,
            array,
        })
    }

    /// The yes/no answers lowering may depend on for `t` (`Key::bits`).
    fn bits(&mut self, t: TyId) -> u32 {
        let answers = [
            self.needs_drop(t),
            self.has_null_niche(t),
            self.is_class(t),
            self.boxed(t),
            self.counted(t),
            self.proxiable(t),
            self.unique_refs(t),
            self.lock_counted(t),
            self.weak_capable(t),
            self.is_union(t),
        ];
        answers
            .iter()
            .enumerate()
            .fold(0, |acc, (i, &b)| acc | u32::from(b) << i)
    }

    /// Does `t` mention a type parameter (a generic leftover, or a placeholder)?
    fn mentions_any_param(&self, t: TyId) -> bool {
        let mut stack = vec![t];
        let mut seen = FxHashSet::default();
        while let Some(x) = stack.pop() {
            if !seen.insert(x) {
                continue;
            }
            match self.types.kind(x) {
                TyKind::Param(_) => return true,
                k => stack.extend(super::parts(k)),
            }
        }
        false
    }

    /// `t` with each type `from` of `map` replaced by its `to`.
    pub(in crate::lower) fn replace_tys(&mut self, t: TyId, map: &[(TyId, TyId)]) -> TyId {
        if let Some(&(_, to)) = map.iter().find(|(from, _)| *from == t) {
            return to;
        }
        let r = |cx: &mut Self, x: TyId| cx.replace_tys(x, map);
        let k = match self.types.kind(t).clone() {
            TyKind::Adt(d, a) => TyKind::Adt(d, a.iter().map(|&x| r(self, x)).collect()),
            TyKind::Dyn(d, a) => TyKind::Dyn(d, a.iter().map(|&x| r(self, x)).collect()),
            TyKind::Tuple(a) => TyKind::Tuple(a.iter().map(|&x| r(self, x)).collect()),
            TyKind::Array(e) => TyKind::Array(r(self, e)),
            TyKind::Option(e, n) => TyKind::Option(r(self, e), n),
            TyKind::Shared(e) => TyKind::Shared(r(self, e)),
            TyKind::Map(a, b) => TyKind::Map(r(self, a), r(self, b)),
            TyKind::Result(a, b) => TyKind::Result(r(self, a), r(self, b)),
            TyKind::Promise(a, b) => TyKind::Promise(r(self, a), r(self, b)),
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => TyKind::FnPtr {
                params: params.iter().map(|&x| r(self, x)).collect(),
                ret: r(self, ret),
                throws: r(self, throws),
                rest,
            },
            _ => return t,
        };
        self.intern(k)
    }

    /// Put each type built from representative `rep` that a counted-types set holds into it
    /// again, built from placeholder `p` instead.
    pub(super) fn mirror(&mut self, p: TyId, rep: TyId) {
        let sets = self.boxing.ty_sets().len();
        for i in 0..sets {
            let members: Vec<TyId> = self.boxing.ty_sets()[i].iter().copied().collect();
            for m in members {
                let m2 = self.replace_tys(m, &[(rep, p)]);
                if m2 != m && self.boxing.ty_sets_mut()[i].insert(m2) {
                    self.shapes.mirrored.push((i, m2));
                }
            }
        }
    }

    /// Take the placeholders' entries out of the counted-types sets again (before the pass
    /// closes its facts) after checking that every member of every shape agrees with the
    /// representative on each type built from the placeholder (module docs).
    pub(in crate::lower) fn shapes_close(&mut self) {
        self.verify();
        for (i, t) in std::mem::take(&mut self.shapes.mirrored) {
            self.boxing.ty_sets_mut()[i].remove(&t);
        }
    }

    fn verify(&mut self) {
        let n = self.types.len();
        let derived: Vec<TyId> = (0..n as u32)
            .map(TyId)
            .filter(|&t| self.mentions_ph(t) && !self.is_ph(t))
            .collect();
        for i in 0..self.shapes.ph.len() {
            let (p, rep) = (self.shapes.ph[i].ty, self.shapes.ph[i].rep);
            let members = self.shapes.ph[i].members.clone();
            for &d in &derived {
                let rest: Vec<(TyId, TyId)> = self.shapes.ph.iter().map(|q| (q.ty, q.rep)).collect();
                let want = self.replace_tys(d, &rest);
                let want = self.set_answers(want);
                for &m in &members {
                    let mut map = rest.clone();
                    map[i] = (p, m);
                    let got = self.replace_tys(d, &map);
                    if self.set_answers(got) != want {
                        let (a, b) = (self.type_name(rep), self.type_name(m));
                        ice(format_args!(
                            "shape of `{a}` and `{b}` differs for a type built from them: \
                             the shape key is missing a question"
                        ));
                    }
                }
            }
        }
    }

    /// Which counted-types sets hold `t`, and the answers of `bits`.
    fn set_answers(&mut self, t: TyId) -> (Vec<bool>, u32) {
        let sets = self.boxing.ty_sets().iter().map(|s| s.contains(&t)).collect();
        (sets, self.bits(t))
    }
}
