//! The assignability relation of conditional types (`C extends E`), with `infer` placeholders
//! bound on the way, as TypeScript relates types: literals to their base, unions member-wise,
//! object types structurally.

use velt_common::Span;

use super::{INFER_BASE, WILDCARD};
use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::hir::{AdtKind, Nothing, TyId, TyKind};

impl Ctx<'_> {
    /// Does `null` / `undefined` extend `p`?
    pub(super) fn relate_nothing(&self, n: Nothing, p: TyId) -> bool {
        match *self.ty.kind(p) {
            TyKind::Param(WILDCARD) => true,
            TyKind::Option(_, m) => n.within(m),
            _ => false,
        }
    }

    /// Is `s` assignable to `p` (TypeScript's relation, structural for object types), binding
    /// the `infer` placeholders `p` holds into `slots`?
    pub(super) fn relate(&mut self, s: TyId, p: TyId, slots: &mut Vec<Option<TyId>>) -> bool {
        if s == p {
            return true;
        }
        let pk = self.ty.kind(p).clone();
        if let TyKind::Param(i) = pk {
            if i == WILDCARD {
                return true;
            }
            if i >= INFER_BASE {
                let slot = &mut slots[(i - INFER_BASE) as usize];
                *slot = Some(match *slot {
                    Some(prev) if prev != s => self.union_with(&[prev, s], None, Span::default()),
                    _ => s,
                });
                return true;
            }
        }
        if s == self.ty.never || s == self.ty.error || p == self.ty.error {
            return true;
        }
        let sk = self.ty.kind(s).clone();
        if let TyKind::Option(sp, sn) = sk {
            let nothings: &[Nothing] = match sn {
                Nothing::Both => &[Nothing::Null, Nothing::Undefined],
                Nothing::Null => &[Nothing::Null],
                Nothing::Undefined => &[Nothing::Undefined],
            };
            return nothings
                .iter()
                .all(|n| self.relate_nothing(*n, p) || self.binds_nothing(p))
                && self.relate(sp, p, slots);
        }
        if let Some(ms) = self.union_members(s) {
            return ms.into_iter().all(|m| self.relate(m, p, slots));
        }
        if let TyKind::Option(pp, _) = pk {
            return self.relate(s, pp, slots);
        }
        if let Some(ms) = self.union_members(p) {
            for m in ms {
                let saved = slots.clone();
                if self.relate(s, m, slots) {
                    return true;
                }
                *slots = saved;
            }
            return false;
        }
        match (sk, pk) {
            (TyKind::Literal(v), _) => self.lit_base(&v) == p,
            (TyKind::Array(a), TyKind::Array(b)) => self.relate(a, b, slots),
            (TyKind::Tuple(a), TyKind::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| self.relate(*x, *y, slots))
            }
            (TyKind::Tuple(a), TyKind::Array(b)) => a.iter().all(|x| self.relate(*x, b, slots)),
            (TyKind::Promise(a, _), TyKind::Promise(b, _))
            | (TyKind::Shared(a), TyKind::Shared(b)) => self.relate(a, b, slots),
            (TyKind::Map(a1, a2), TyKind::Map(b1, b2)) => {
                self.relate(a1, b1, slots) && self.relate(a2, b2, slots)
            }
            (
                TyKind::FnPtr {
                    params: sp,
                    ret: sr,
                    ..
                },
                TyKind::FnPtr {
                    params: pp,
                    ret: pr,
                    ..
                },
            ) => {
                // Parameters are compared both ways (TypeScript's method bivariance), which
                // also lets `infer` find a parameter's type.
                sp.len() <= pp.len()
                    && sp.iter().zip(&pp).all(|(x, y)| {
                        let saved = slots.clone();
                        self.relate(*x, *y, slots) || {
                            *slots = saved;
                            self.relate(*y, *x, slots)
                        }
                    })
                    && self.relate(sr, pr, slots)
            }
            (_, TyKind::Adt(pd, _)) if self.is_structural_object(pd) => {
                self.relate_fields(s, p, slots)
            }
            (TyKind::Adt(sd, sa), TyKind::Adt(pd, pa))
            | (TyKind::Dyn(sd, sa), TyKind::Dyn(pd, pa))
                if sd == pd && sa.len() == pa.len() =>
            {
                sa.iter().zip(&pa).all(|(x, y)| self.relate(*x, *y, slots))
            }
            _ => false,
        }
    }

    /// Is `p` an `infer` placeholder or the wildcard (which take `null` and `undefined` too)?
    fn binds_nothing(&self, p: TyId) -> bool {
        matches!(*self.ty.kind(p), TyKind::Param(i) if i >= WILDCARD)
    }

    /// An anonymous object type or a field-only interface's: related structurally.
    fn is_structural_object(&self, d: crate::hir::DefId) -> bool {
        self.adt(d)
            .is_some_and(|a| a.kind == AdtKind::Anon || self.field_only_of.contains_key(&d))
    }

    /// `s` has every field object type `p` requires, each assignable to `p`'s.
    fn relate_fields(&mut self, s: TyId, p: TyId, slots: &mut Vec<Option<TyId>>) -> bool {
        let (Some(pf), Some(sf)) = (self.plain_fields(p), self.plain_fields(s)) else {
            return false;
        };
        for f in pf {
            match sf.iter().find(|g| g.name == f.name) {
                Some(g) => {
                    let st = match g.optional && !f.optional {
                        true => self.ty.option_of(g.ty, Nothing::Undefined),
                        false => g.ty,
                    };
                    if !self.relate(st, f.ty, slots) {
                        return false;
                    }
                }
                None if f.optional => {}
                None => return false,
            }
        }
        true
    }

    /// The public fields of object type `t` (anonymous, a field-only interface, a class or a
    /// struct), without reporting anything when `t` has none.
    pub(crate) fn plain_fields(&mut self, t: TyId) -> Option<Vec<ShapeField>> {
        let TyKind::Adt(d, args) = self.ty.kind(t).clone() else {
            return None;
        };
        self.adt(d)?;
        let fields = self.fields_now(d).ok()?;
        Some(
            fields
                .into_iter()
                .filter(|(_, public)| *public)
                .map(|(f, _)| ShapeField {
                    ty: self.subst(f.ty, &args),
                    ..f
                })
                .collect(),
        )
    }
}
