//! The assignability relation of conditional types (`C extends E`), with `infer` placeholders
//! bound on the way, as TypeScript relates types under `--strict`: literals to their base,
//! `boolean` as `true | false`, unions member-wise, object types structurally, function
//! parameters contravariantly.

use velt_common::Span;

use super::{INFER_BASE, UNKNOWN, WILDCARD};
use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::hir::{AdtKind, LitValue, Nothing, TyId, TyKind};

/// What each `infer X` of a pattern found while it is related: candidates in covariant
/// positions (a union) and in contravariant ones (function parameters: an intersection).
#[derive(Clone, Default)]
pub(super) struct Slots {
    co: Vec<Option<TyId>>,
    contra: Vec<Vec<TyId>>,
    /// Relating inside a function parameter (an odd number of them).
    flipped: bool,
}

impl Slots {
    pub(super) fn new(n: usize) -> Self {
        Slots {
            co: vec![None; n],
            contra: vec![vec![]; n],
            flipped: false,
        }
    }
}

impl Ctx<'_> {
    /// Does `null` / `undefined` extend `p`?
    pub(super) fn relate_nothing(&self, n: Nothing, p: TyId) -> bool {
        match *self.ty.kind(p) {
            TyKind::Param(WILDCARD | UNKNOWN) => true,
            TyKind::Option(_, m) => n.within(m),
            _ => false,
        }
    }

    /// Is `s` assignable to `p` (TypeScript's relation, structural for object types), binding
    /// the `infer` placeholders `p` holds into `slots`?
    pub(super) fn relate(&mut self, s: TyId, p: TyId, slots: &mut Slots) -> bool {
        if s == p {
            return true;
        }
        let pk = self.ty.kind(p).clone();
        if let TyKind::Param(i) = pk {
            if i == WILDCARD || i == UNKNOWN {
                return true;
            }
            if i >= INFER_BASE {
                let i = (i - INFER_BASE) as usize;
                if slots.flipped {
                    if !slots.contra[i].contains(&s) {
                        slots.contra[i].push(s);
                    }
                    return true;
                }
                let slot = &mut slots.co[i];
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
        match sk {
            // `any` is assignable to everything, `unknown` only to `any` and `unknown` (above).
            TyKind::Param(WILDCARD) => return true,
            TyKind::Param(UNKNOWN) => return false,
            // `boolean` is `true | false`.
            TyKind::Bool if !matches!(pk, TyKind::Bool) => {
                let t = self.lit_type(LitValue::Bool(true));
                let f = self.lit_type(LitValue::Bool(false));
                return self.relate(t, p, slots) && self.relate(f, p, slots);
            }
            _ => {}
        }
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
            (_, TyKind::Adt(pd, _)) if self.is_structural_object(pd) => {
                self.relate_fields(s, p, slots)
            }
            // A number literal is a `number` (Velt's integer literal types included).
            (TyKind::Literal(LitValue::Int(..)), TyKind::Float(_) | TyKind::Int(_))
            | (TyKind::Literal(LitValue::Float(..)), TyKind::Float(_)) => true,
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
                // Parameters relate contravariantly (`--strict`'s strictFunctionTypes): the
                // pattern's parameter must be assignable to the subject's. What an `infer X`
                // in the pattern's parameter finds is a contravariant candidate.
                sp.len() <= pp.len()
                    && sp.iter().zip(&pp).all(|(x, y)| {
                        if self.has_infer(*y) {
                            slots.flipped = !slots.flipped;
                            let ok = self.relate(*x, *y, slots);
                            slots.flipped = !slots.flipped;
                            ok
                        } else {
                            self.relate(*y, *x, &mut Slots::default())
                        }
                    })
                    && self.relate(sr, pr, slots)
            }
            (_, TyKind::Adt(..)) if self.record_type_args(p).is_some() => {
                self.relate_to_record(s, p, slots)
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

    /// Is `p` an `infer` placeholder or a wildcard (which take `null` and `undefined` too)?
    fn binds_nothing(&self, p: TyId) -> bool {
        matches!(*self.ty.kind(p), TyKind::Param(i) if i >= UNKNOWN)
    }

    /// Does `t` hold an `infer` placeholder?
    fn has_infer(&self, t: TyId) -> bool {
        let mut ps = vec![];
        crate::types::collect_params(&self.ty, t, &mut ps);
        ps.iter().any(|p| *p >= INFER_BASE)
    }

    /// What each `infer X` found: its covariant candidates' union, else its contravariant
    /// candidates' intersection (`string & number` is `never`); `None` for nothing. `Err` after
    /// an error (reported at `span`).
    pub(super) fn finish_slots(
        &mut self,
        slots: Slots,
        module: usize,
        span: Span,
    ) -> Result<Vec<Option<TyId>>, ()> {
        let mut out = vec![];
        for (co, contra) in slots.co.into_iter().zip(slots.contra) {
            if co.is_some() || contra.is_empty() {
                out.push(co);
                continue;
            }
            let mut acc = contra[0];
            for &b in &contra[1..] {
                acc = match self.meet(acc, b, module, 0) {
                    Ok(m) => m,
                    Err(_) if !self.is_object_type(acc) && !self.is_object_type(b) => self.ty.never,
                    Err(why) => {
                        self.err(
                            format!("`infer` found object types with no value in common ({why}); their intersection is not supported yet (#934)"),
                            span,
                        );
                        return Err(());
                    }
                };
            }
            out.push(Some(acc));
        }
        Ok(out)
    }

    /// An anonymous object type or a field-only interface's: related structurally.
    fn is_structural_object(&self, d: crate::hir::DefId) -> bool {
        self.adt(d)
            .is_some_and(|a| a.kind == AdtKind::Anon || self.field_only_of.contains_key(&d))
    }

    /// `s` has every field object type `p` requires, each assignable to `p`'s. The empty
    /// object type (`{}`) takes any value but `null` and `undefined` (related before).
    fn relate_fields(&mut self, s: TyId, p: TyId, slots: &mut Slots) -> bool {
        let Some(pf) = self.plain_fields(p) else {
            return false;
        };
        if pf.is_empty() {
            return true;
        }
        let Some(sf) = self.relatable_fields(s) else {
            return false;
        };
        for f in pf {
            match sf.iter().find(|g| g.name == f.name) {
                // An optional field does not fill a required one, even one that takes
                // `undefined`; an optional one takes `undefined` (no exactOptionalPropertyTypes).
                Some(g) if g.optional && !f.optional => return false,
                Some(g) => {
                    let pt = match f.optional {
                        true => self.union_with(&[f.ty], Some(Nothing::Undefined), Span::DUMMY),
                        false => f.ty,
                    };
                    if !self.relate(g.ty, pt, slots) {
                        return false;
                    }
                }
                None if f.optional => {}
                None => return false,
            }
        }
        true
    }

    /// The fields `s` has as an object type: an object type's or class's, or a record's with
    /// literal keys (each required).
    fn relatable_fields(&mut self, s: TyId) -> Option<Vec<ShapeField>> {
        if let Some((k, v)) = self.record_type_args(s) {
            let keys = self.record_key_names(k)?;
            return Some(
                keys.into_iter()
                    .map(|name| ShapeField {
                        name,
                        ty: v,
                        readonly: false,
                        optional: false,
                    })
                    .collect(),
            );
        }
        self.plain_fields(s)
    }

    /// `s` against `Record<K, V>`: a closed record's keys are required fields of type `V`; an
    /// open one (`Record<string, V>`) takes an object type whose fields are all `V`s.
    fn relate_to_record(&mut self, s: TyId, p: TyId, slots: &mut Slots) -> bool {
        let Some((k, v)) = self.record_type_args(p) else {
            return false;
        };
        if let Some((sk, sv)) = self.record_type_args(s) {
            return self.relate(k, sk, &mut Slots::default()) && self.relate(sv, v, slots);
        }
        let Some(sf) = self.relatable_fields(s) else {
            return false;
        };
        match self.record_key_names(k) {
            Some(keys) => keys.iter().all(|key| {
                sf.iter()
                    .find(|g| g.name == *key)
                    .is_some_and(|g| !g.optional && self.relate(g.ty, v, slots))
            }),
            None if *self.ty.kind(k) == TyKind::Str => {
                sf.iter().all(|g| self.relate(g.ty, v, slots))
            }
            None => false,
        }
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
