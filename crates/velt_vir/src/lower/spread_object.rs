//! `Intrinsic::SpreadObject` (#350; hir_encodings.md "Spreads in generic bodies"): an object
//! literal that spreads a value whose fields are known only per instantiation (`{ ...t, $set }`
//! with `t: T`). Its type is the deferred `TyOp::Spread`, which sema reduced at this
//! instantiation to the object type JavaScript's spread builds. Each part is a place of a
//! concrete object type (a spread source, or a temporary holding a run of explicit properties);
//! each field of the result is shared from the last part that has a field of that name, and an
//! optional field (which sema allows only from one part) keeps that part's presence.

use velt_sema::hir::{self, LitValue, TyId, TyKind};

use super::{ice, unit, FnLower};
use crate::vir::{Operand, Place, Rvalue, Ty};

impl FnLower<'_, '_> {
    pub(super) fn spread_object(&mut self, parts: &[hir::Expr], ty: TyId) -> Operand {
        let rty = self.sub(ty);
        let srcs: Vec<(Place, TyId)> = parts
            .iter()
            .map(|p| (self.place_expr(p), self.sub(p.ty)))
            .collect();
        let TyKind::Adt(d, _) = self.cx.kind(rty) else {
            ice(format_args!(
                "a spread object of the non-object type {rty:?}"
            ))
        };
        let names: Vec<String> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let tys = self.cx.adt_field_tys(rty);
        let mut ops = vec![];
        let mut flags = vec![];
        for (i, name) in names.iter().enumerate() {
            // The parts that have the field, latest first.
            let from: Vec<(Place, TyId, u32)> = srcs
                .iter()
                .rev()
                .filter_map(|(p, sty)| {
                    let TyKind::Adt(sd, _) = self.cx.kind(*sty) else {
                        return None;
                    };
                    let k = (self.cx.adt_def(sd).fields.iter()).position(|f| f.name == *name)?;
                    Some((p.clone(), *sty, k as u32))
                })
                .collect();
            if from.is_empty() {
                ice(format_args!("spread field `{name}` has no source"))
            }
            let fty = tys[i];
            let flagged = self.cx.presence_slot(rty, i as u32).is_some();
            if self.cx.is_unit(fty) {
                ops.push(unit());
                if flagged {
                    flags.push(FnLower::ctrue());
                }
                continue;
            }
            let (v, f) = self.spread_field(&from, fty, flagged, name);
            ops.push(v);
            flags.extend(f);
        }
        ops.extend(flags);
        self.build_agg(rty, ops)
    }

    /// The value of a spread field of type `fty` (and its presence flag, when `flagged`), from
    /// `from`, the parts that have it, latest first: a part whose field may be absent gives it
    /// only while it is present, else an earlier one does, as JavaScript's spread copies only
    /// the keys an object has (a present `undefined` too).
    fn spread_field(
        &mut self,
        from: &[(Place, TyId, u32)],
        fty: TyId,
        flagged: bool,
        name: &str,
    ) -> (Operand, Option<Operand>) {
        let vt = self.vty(fty);
        let out = self.temp(vt);
        let flag = flagged.then(|| self.temp(Ty::Bool));
        let join = self.new_block();
        for (n, (place, sty, k)) in from.iter().enumerate() {
            let fp = self.field_place(place, *sty, *k);
            let slot = self.cx.presence_slot(*sty, *k);
            let present = match slot {
                Some(slot) => {
                    let mut p = fp.clone();
                    if let Some(crate::vir::Proj::Field(f)) = p.proj.last_mut() {
                        *f = slot;
                    }
                    Operand::Copy(p)
                }
                None => FnLower::ctrue(),
            };
            let last = n + 1 == from.len();
            let next = (!last && slot.is_some()).then(|| self.new_block());
            if let Some(next) = next {
                let then = self.new_block();
                self.branch(present.clone(), then, next);
                self.switch_to(then);
            }
            let sft = self.cx.member_ty(*sty, *k);
            let value = if self.vty(sft) == vt {
                Operand::Copy(fp)
            } else {
                // `a?: F` over a required `a: F`: a present `undefined` can't be kept.
                let ok = self.option_is_some(&fp, sft);
                let (good, bad) = (self.new_block(), self.new_block());
                self.branch(ok, good, bad);
                self.switch_to(bad);
                self.panic_msg(&format!(
                    "TypeError: a spread copies `undefined` into the required field `{name}`"
                ));
                self.terminate(crate::vir::Terminator::Unreachable);
                self.switch_to(good);
                Operand::Copy(self.some_payload(&fp, sft))
            };
            let v = self.share_value(value, fty);
            self.assign(Place::local(out), Rvalue::Use(v));
            if let Some(f) = flag {
                let p = if self.vty(sft) == vt {
                    present
                } else {
                    FnLower::ctrue()
                };
                self.assign(Place::local(f), Rvalue::Use(p));
            }
            self.goto(join);
            match next {
                Some(next) => self.switch_to(next),
                None => break,
            }
        }
        self.switch_to(join);
        (
            Operand::Copy(Place::local(out)),
            flag.map(|f| Operand::Copy(Place::local(f))),
        )
    }

    /// The field index of the key read or written by `object[key]` at this instantiation: the
    /// key's type is one string literal naming a field of the object's type (sema checks it).
    fn key_field(&mut self, object: &hir::Expr, key: &hir::Expr) -> (TyId, u32) {
        let oty = self.sub(object.ty);
        let kty = self.sub(key.ty);
        let (TyKind::Adt(d, _), TyKind::Literal(LitValue::Str(name))) =
            (self.cx.kind(oty), self.cx.kind(kty))
        else {
            ice(format_args!("a key access of {oty:?} by {kty:?}"))
        };
        let Some(k) = (self.cx.adt_def(d).fields.iter()).position(|f| f.name == name) else {
            ice(format_args!("a key access of the missing field `{name}`"))
        };
        (oty, k as u32)
    }

    /// `Intrinsic::KeyRead`: `object[key]`, the field shared, as `object.<key>` reads it.
    pub(super) fn key_read(&mut self, object: &hir::Expr, key: &hir::Expr, ty: TyId) -> Operand {
        let (oty, k) = self.key_field(object, key);
        let fty = self.sub(ty);
        self.expr(key);
        let place = self.place_expr(object);
        if self.cx.is_unit(fty) {
            return unit();
        }
        let fp = self.field_place(&place, oty, k);
        self.share_value(Operand::Copy(fp), fty)
    }

    /// `Intrinsic::KeyWrite`: `object[key] = value`, as `object.<key> = value` writes it.
    pub(super) fn key_write(
        &mut self,
        object: &hir::Expr,
        key: &hir::Expr,
        value: &hir::Expr,
    ) -> Operand {
        let (_, k) = self.key_field(object, key);
        self.expr(key);
        let field = hir::Expr {
            kind: hir::ExprKind::Field {
                base: Box::new(object.clone()),
                index: k,
                mode: hir::UseMode::BorrowMut,
            },
            ty: value.ty,
            span: value.span,
        };
        self.assign_expr(&field, value)
    }
}
