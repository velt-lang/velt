//! The assignability relation of conditional types (`C extends E`), with `infer` placeholders
//! bound on the way, as TypeScript relates types under `--strict`: literals to their base,
//! `boolean` as `true | false`, unions member-wise, object types structurally, function
//! parameters contravariantly.

use velt_common::Span;

use super::{INFER_BASE, UNKNOWN, WILDCARD};
use crate::anon::ShapeField;
use crate::any::Dynamic;
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
        if matches!(self.dynamic(p), Some(Dynamic::Any | Dynamic::Unknown)) {
            return true;
        }
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
        // `any` and `unknown` written in a type bound (patterns have placeholders for them).
        if matches!(self.dynamic(p), Some(Dynamic::Any | Dynamic::Unknown)) {
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
            (_, TyKind::Dyn(..)) if self.dynamic(p) == Some(Dynamic::Object) => {
                self.is_object_ty(s)
            }
            (TyKind::FnPtr { .. } | TyKind::Closure(_), TyKind::Dyn(..))
                if self.dynamic(p) == Some(Dynamic::Function) =>
            {
                true
            }
            (_, TyKind::Adt(pd, _)) if self.is_structural_object(pd) => {
                self.relate_fields(s, p, slots)
            }
            // A number literal is a `number` (Velt's integer literal types included).
            (TyKind::Literal(LitValue::Int(..)), TyKind::Float(_) | TyKind::Int(_))
            | (TyKind::Literal(LitValue::Float(..)), TyKind::Float(_)) => true,
            // `1` and `1.0` are one literal type in TypeScript: number literals compare by value.
            (TyKind::Literal(a), TyKind::Literal(b)) => {
                matches!((num_value(&a), num_value(&b)), (Some(x), Some(y)) if x == y)
            }
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
                    rest: s_rest,
                    ..
                },
                TyKind::FnPtr {
                    params: pp,
                    ret: pr,
                    rest: p_rest,
                    ..
                },
            ) => {
                let ok = match (s_rest, p_rest) {
                    (false, true) => self.relate_to_rest(&sp, &pp, slots),
                    (true, _) => self.relate_from_rest(&sp, &pp, p_rest, slots),
                    (false, false) => {
                        sp.len() <= pp.len()
                            && sp
                                .iter()
                                .zip(&pp)
                                .all(|(x, y)| self.relate_param(*x, *y, slots))
                    }
                };
                ok && self.relate(sr, pr, slots)
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
            // A subclass is its base class (`T extends Base` takes a `Sub`, `E extends Error`
            // a `class NotFound extends Error`).
            (TyKind::Adt(sd, _), TyKind::Adt(pd, _)) if self.class_extends(sd, pd) => {
                match self.base_of(s) {
                    Some(b) => self.relate(b, p, slots),
                    None => false,
                }
            }
            _ => false,
        }
    }

    /// Does type argument `s` satisfy the type bound `c` (`T extends c`, `Generics::constraints`)
    /// as TypeScript relates them? `c` with type parameters still unknown is not tested.
    pub(crate) fn satisfies_constraint(&mut self, s: TyId, c: TyId) -> bool {
        if self.dynamic(s) == Some(Dynamic::Any) || self.has_type_params(c) {
            return true;
        }
        self.relate(s, c, &mut Slots::default())
    }

    /// Is `p` an `infer` placeholder or a wildcard (which take `null` and `undefined` too)?
    fn binds_nothing(&self, p: TyId) -> bool {
        matches!(*self.ty.kind(p), TyKind::Param(i) if i >= UNKNOWN)
    }

    /// Parameters relate contravariantly (`--strict`'s strictFunctionTypes): the pattern's
    /// parameter `p` must be assignable to the subject's `s`. What an `infer X` in the pattern's
    /// parameter finds is a contravariant candidate.
    fn relate_param(&mut self, s: TyId, p: TyId, slots: &mut Slots) -> bool {
        // A pattern's `any` parameter, or `any` / `unknown` rest element, takes any argument.
        let any = |cx: &Self, t: TyId| {
            matches!(cx.dynamic(t), Some(Dynamic::Any))
                || matches!(*cx.ty.kind(t), TyKind::Param(WILDCARD))
        };
        let top = |cx: &Self, t: TyId| {
            any(cx, t)
                || matches!(cx.dynamic(t), Some(Dynamic::Unknown))
                || matches!(*cx.ty.kind(t), TyKind::Param(UNKNOWN))
        };
        if any(self, p) || self.ty.array_elem(p).is_some_and(|e| top(self, e)) {
            return true;
        }
        if self.has_infer(p) {
            slots.flipped = !slots.flipped;
            let ok = self.relate(s, p, slots);
            slots.flipped = !slots.flipped;
            ok
        } else {
            self.relate(p, s, &mut Slots::default())
        }
    }

    /// The parameters `sp` of a function type without a rest parameter against a pattern's
    /// `pp` ending in one (`(...args: any[]) => any`, `(e: E, ...args: infer A) => void`): the
    /// subject's parameters from the rest parameter's position on are its elements, or the
    /// tuple an `infer` there finds (TypeScript's `Parameters<F>`).
    fn relate_to_rest(&mut self, sp: &[TyId], pp: &[TyId], slots: &mut Slots) -> bool {
        let Some((&last, fixed)) = pp.split_last() else {
            return false;
        };
        let (head, tail) = sp.split_at(fixed.len().min(sp.len()));
        if !head
            .iter()
            .zip(fixed)
            .all(|(x, y)| self.relate_param(*x, *y, slots))
        {
            return false;
        }
        if self.has_infer(last) {
            let tail = self.ty.intern(TyKind::Tuple(tail.to_vec()));
            return self.relate_param(tail, last, slots);
        }
        match self.ty.array_elem(last) {
            Some(e) => tail.iter().all(|x| self.relate_param(*x, e, slots)),
            None => false,
        }
    }

    /// Does `t` hold an `infer` placeholder?
    /// The parameters `sp` of a function type ending in a rest parameter against a pattern's
    /// `pp` (ending in one when `p_rest`): the subject takes any number of arguments from its
    /// rest parameter's position on, each an element of its rest array. A pattern rest parameter
    /// that is an `infer` (`(...a: infer A) => R`) finds the subject's rest array when the
    /// subject has no other parameters past the pattern's fixed ones; with them it would be a
    /// variadic tuple, which Velt doesn't have.
    fn relate_from_rest(
        &mut self,
        sp: &[TyId],
        pp: &[TyId],
        p_rest: bool,
        slots: &mut Slots,
    ) -> bool {
        let Some((&s_last, s_fixed)) = sp.split_last() else {
            return false;
        };
        let Some(s_elem) = self.ty.array_elem(s_last) else {
            return false;
        };
        let (p_fixed, p_last) = match (p_rest, pp.split_last()) {
            (true, Some((&last, fixed))) => (fixed, Some(last)),
            _ => (pp, None),
        };
        // Each pattern fixed parameter against the subject's at its position, or its element.
        for (i, &y) in p_fixed.iter().enumerate() {
            let x = s_fixed.get(i).copied().unwrap_or(s_elem);
            if !self.relate_param(x, y, slots) {
                return false;
            }
        }
        let Some(p_last) = p_last else {
            return true;
        };
        let extra = s_fixed.get(p_fixed.len()..).unwrap_or_default();
        if self.has_infer(p_last) {
            return extra.is_empty() && self.relate_param(s_last, p_last, slots);
        }
        let Some(p_elem) = self.ty.array_elem(p_last) else {
            return false;
        };
        extra.iter().all(|x| self.relate_param(*x, p_elem, slots))
            && self.relate_param(s_last, p_last, slots)
    }

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
        // A weak type (all fields optional) takes only a source with no properties or one that
        // shares a property with it (TypeScript's weak type detection).
        if pf.iter().all(|f| f.optional)
            && !sf.is_empty()
            && !sf.iter().any(|g| pf.iter().any(|f| f.name == g.name))
        {
            return false;
        }
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
    pub(crate) fn relatable_fields(&mut self, s: TyId) -> Option<Vec<ShapeField>> {
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
        // Two records relate as TypeScript relates two instances of one alias: by their type
        // arguments first (the key contravariantly, the value covariantly), else structurally
        // (a closed record as its fields; an open one has no property a closed one requires).
        if let Some((sk, sv)) = self.record_type_args(s) {
            let saved = slots.clone();
            if self.relate(k, sk, &mut Slots::default()) && self.relate(sv, v, slots) {
                return true;
            }
            *slots = saved;
            if self.record_key_names(sk).is_none() {
                return false;
            }
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

/// The value of a number literal (`1` and `1.0` are the same literal type in TypeScript).
pub(super) fn num_value(v: &LitValue) -> Option<f64> {
    match v {
        LitValue::Int(_, i) => Some(*i as f64),
        LitValue::Float(_, bits) => Some(f64::from_bits(*bits)),
        _ => None,
    }
}
