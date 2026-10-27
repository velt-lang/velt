//! Spreads of values whose fields are known only per instantiation, and conversions between
//! types of a generic body that are one object type at every instantiation (#350;
//! `crate::generic_spread`, hir_encodings.md "Spreads in generic bodies").

use velt_syntax::ast;

use super::eval_effects::Effects;
use crate::body::places::{is_place, set_place_mode};
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Intrinsic, TyId, TyKind, TyOp, UseMode};

impl FnCx<'_, '_> {
    /// Is `t` a type whose fields are known only per instantiation: a type parameter, or a
    /// deferred intersection or spread?
    pub(super) fn deferred_object(&self, t: TyId) -> bool {
        matches!(
            self.cx.ty.kind(t),
            TyKind::Param(_) | TyKind::Op(TyOp::Intersect | TyOp::Spread, _)
        )
    }

    /// Does object literal `props` spread a value whose fields are known only per instantiation?
    pub(super) fn spreads_deferred(&mut self, props: &[ast::ObjectProp]) -> bool {
        props.iter().any(|p| match p {
            ast::ObjectProp::Spread(e) => self.peek_ty(e).is_some_and(|t| self.deferred_object(t)),
            _ => false,
        })
    }

    /// `{ ...t, $set }` with `t: T`: an `Intrinsic::SpreadObject` over its parts, of the
    /// deferred type `TyOp::Spread`. Every part is bound to a temporary in written order, as
    /// JavaScript evaluates them: a spread source (a place is borrowed as it is, or copied there
    /// when a later part may change it: `{ ...t, y: f(t) }`) and each run of
    /// explicit properties as a one-field-per-property object literal. `exp` gives explicit
    /// properties their expected types (`$set: (v) => …` against `T & { $set(v: T): void }`).
    pub(super) fn deferred_spread(
        &mut self,
        props: &[ast::ObjectProp],
        exp: Option<TyId>,
        span: velt_common::Span,
    ) -> hir::Expr {
        let mut lets = vec![];
        let mut parts: Vec<hir::Expr> = vec![];
        let mut run: Vec<(String, hir::Expr)> = vec![];
        let expected = exp.map(|t| self.object_parts(t)).unwrap_or_default();
        // The places spread (their parts, and the statements bound before them), and whether a
        // later part may change them.
        let mut places: Vec<(usize, usize, bool)> = vec![];
        for p in props {
            let (key, value) = match p {
                ast::ObjectProp::Spread(e) => {
                    self.flush_props(&mut run, &mut parts, &mut lets, span);
                    let h = self.expr(e, None, Want::Borrow);
                    let seen = crate::object_copies::Seen::Spread;
                    self.cx.object_copies.observe(h.ty, e.span, seen);
                    // A source that may be `undefined` or `null` (`...v`, `v: V | undefined`):
                    // the object of its fields, each absent while it is missing.
                    let h = match self.cx.has_type_params(h.ty) {
                        true => h,
                        false => match self.optional_part(h, e.span, &mut lets) {
                            Ok(o) => o,
                            Err(h) => h,
                        },
                    };
                    let mut base = match is_place(&h) {
                        true => {
                            places.push((parts.len(), lets.len(), false));
                            h
                        }
                        false => {
                            if self.hir_effects(&h) == Effects::Writes {
                                places.iter_mut().for_each(|p| p.2 = true);
                            }
                            self.temp("<spread>", h, &mut lets)
                        }
                    };
                    set_place_mode(&mut base, UseMode::Borrow);
                    parts.push(base);
                    continue;
                }
                ast::ObjectProp::KeyValue(k, v) => (k, Some(v)),
                ast::ObjectProp::Shorthand(k) => (k, None),
                ast::ObjectProp::Method(_) => {
                    self.cx.err(
                        "a method in an object literal that spreads a type parameter's value is not supported yet; write it as a property holding an arrow function",
                        span,
                    );
                    continue;
                }
            };
            if run.iter().any(|(n, _)| *n == key.name) {
                let msg = format!("duplicate field `{}` in object literal", key.name);
                self.cx.err(msg, key.span);
                continue;
            }
            let hint = self.field_hint(&expected, &key.name);
            let h = self.prop_value(key, value, hint);
            let h = match hint {
                Some(t) => self.coerce(h, t),
                None => h,
            };
            if self.hir_effects(&h) == Effects::Writes {
                places.iter_mut().for_each(|p| p.2 = true);
            }
            run.push((key.name.clone(), h));
        }
        self.flush_props(&mut run, &mut parts, &mut lets, span);
        // A place a later part may change is copied where it is spread (`{ ...t, y: f(t) }`).
        let mut copies = vec![];
        for (k, at, _) in places.into_iter().filter(|p| p.2) {
            let span = parts[k].span;
            let place = std::mem::replace(&mut parts[k], self.error_expr(span));
            let copy = self.spread_parts(vec![place], span);
            let mut stmts = vec![];
            let mut read = self.temp("<spread>", copy, &mut stmts);
            set_place_mode(&mut read, UseMode::Borrow);
            parts[k] = read;
            copies.push((at, stmts));
        }
        super::written_order::insert_at(&mut lets, copies);
        let value = self.spread_parts(parts, span);
        self.with_lets(lets, value)
    }

    /// The object of `parts`' fields: an `Intrinsic::SpreadObject` of the deferred type
    /// `TyOp::Spread`.
    fn spread_parts(&mut self, parts: Vec<hir::Expr>, span: velt_common::Span) -> hir::Expr {
        let tys: Vec<TyId> = parts.iter().map(|p| p.ty).collect();
        let op = self.cx.ty.intern(TyKind::Op(TyOp::Spread, tys));
        self.cx.type_level.op_sites.entry(op).or_insert(span);
        self.cx
            .type_level
            .op_modules
            .entry(op)
            .or_insert(self.module);
        self.intrinsic(Intrinsic::SpreadObject, parts, op, span)
    }

    /// The explicit properties of `run` (taken) as a part: an object literal bound to a
    /// temporary.
    fn flush_props(
        &mut self,
        run: &mut Vec<(String, hir::Expr)>,
        parts: &mut Vec<hir::Expr>,
        lets: &mut Vec<hir::Stmt>,
        span: velt_common::Span,
    ) {
        if run.is_empty() {
            return;
        }
        let run = std::mem::take(run);
        let fields: Vec<(String, TyId)> = run.iter().map(|(n, h)| (n.clone(), h.ty)).collect();
        let (def, type_args) = self.anon_def(&fields);
        let ty = self.cx.ty.intern(TyKind::Adt(def, type_args.clone()));
        let lit = H::AdtLit {
            def,
            type_args,
            fields: run.into_iter().map(|(_, h)| h).collect(),
            converted: hir::Conversion::No,
        };
        let lit = self.mk(lit, ty, span);
        let mut part = self.temp("<props>", lit, lets);
        set_place_mode(&mut part, UseMode::Borrow);
        parts.push(part);
    }

    /// The parts of `t`: an intersection's or spread's, flattened; `t` itself otherwise.
    pub(super) fn object_parts(&self, t: TyId) -> Vec<TyId> {
        match self.cx.ty.kind(t) {
            TyKind::Op(TyOp::Intersect | TyOp::Spread, ps) => {
                ps.iter().flat_map(|&p| self.object_parts(p)).collect()
            }
            _ => vec![t],
        }
    }

    /// The type field `name` has in the object parts of `parts`, the last one that has it.
    fn field_hint(&mut self, parts: &[TyId], name: &str) -> Option<TyId> {
        parts.iter().rev().find_map(|&p| {
            if self.deferred_object(p) {
                return None;
            }
            let fields = self.cx.public_fields(p);
            let f = fields.into_iter().find(|f| f.name == name)?;
            Some(match f.optional {
                true => self.cx.ty.option_of(f.ty, hir::Nothing::Undefined),
                false => f.ty,
            })
        })
    }

    /// `h`, of a type of this generic body whose fields are known only per instantiation, used
    /// as `exp` (or the reverse) where TypeScript accepts it: every part of `exp` is a part of
    /// `h`'s type, or an object type whose fields `h`'s object parts have. It is the same value
    /// (`Upcast`), with the requirement `TyOp::Convert` that both are one object type at every
    /// instantiation (`crate::generic_spread`).
    pub(super) fn deferred_convert(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        // A value of `T` as a deferred form of `T` itself (`PropsAccessor<T>`, a mapped type
        // over `keyof T`), which TypeScript relates structurally: the same value where the
        // two are one object type, as below.
        let own_form = matches!(self.cx.ty.kind(h.ty), TyKind::Param(_))
            && matches!(self.cx.ty.kind(exp), TyKind::Op(TyOp::Alias(_) | TyOp::Inline(_), args) if args.contains(&h.ty));
        if !own_form
            && (!(self.deferred_object(h.ty) || self.deferred_object(exp))
                || !self.covers(h.ty, exp))
        {
            return Err(h);
        }
        let site = self.cx.type_level.converts.len() as u32;
        let convert = TyKind::Op(TyOp::Convert(site), vec![h.ty, exp]);
        let op = self.cx.ty.intern(convert);
        self.cx.type_level.op_sites.entry(op).or_insert(h.span);
        self.cx
            .type_level
            .op_modules
            .entry(op)
            .or_insert(self.module);
        let span = h.span;
        // A spread used as its target type (`return { ...x, ...patch }` as a `T`) is built as
        // that type, as a concrete spread with an expected type is: a present `undefined` over
        // a field that can't hold one is a `TypeError` there.
        let mut h = h;
        if let Some(at) = retarget_spread(&mut h, exp) {
            self.cx.type_level.spread_into.insert(site);
            self.cx.type_level.converts.insert((exp, exp, at), op);
            return Ok(h);
        }
        self.cx.type_level.converts.insert((h.ty, exp, span), op);
        Ok(self.mk(H::Upcast(Box::new(h)), exp, span))
    }

    /// Does a value of `from` have everything a `to` has, as TypeScript sees it in a generic
    /// body (`T & { a }` has `T` and `{ a }`; `{ ...t, a }` has the same)?
    fn covers(&mut self, from: TyId, to: TyId) -> bool {
        let have = self.object_parts(from);
        let need = self.object_parts(to);
        let mut fields = vec![];
        for &p in &have {
            if !self.deferred_object(p) {
                fields.extend(self.cx.public_fields(p));
            }
        }
        need.iter().all(|&n| {
            if self.deferred_object(n) {
                return have.contains(&n);
            }
            if !self.cx.is_object_type(n) {
                return false;
            }
            let want = self.cx.public_fields(n);
            !want.is_empty()
                && want.iter().all(|w| {
                    fields
                        .iter()
                        .rev()
                        .find(|f| f.name == w.name)
                        .is_some_and(|f| {
                            self.cx.canon(f.ty) == self.cx.canon(w.ty)
                                && (w.optional || !f.optional)
                        })
                })
        })
    }
}

/// `h`, an `Intrinsic::SpreadObject` (in the blocks binding its parts), typed `exp` instead:
/// the span of the spread, or `None` when `h` is no spread.
fn retarget_spread(h: &mut hir::Expr, exp: TyId) -> Option<velt_common::Span> {
    let at = match &mut h.kind {
        H::Block(b) => retarget_spread(b.value.as_mut()?, exp)?,
        H::Call {
            callee: hir::Callee::Intrinsic(Intrinsic::SpreadObject),
            ..
        } => h.span,
        _ => return None,
    };
    h.ty = exp;
    Some(at)
}
