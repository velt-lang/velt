//! `Intrinsic::SpreadObject` (#350; hir_encodings.md "Spreads in generic bodies"): an object
//! literal that spreads a value whose fields are known only per instantiation (`{ ...t, $set }`
//! with `t: T`). Its type is the deferred `TyOp::Spread`, which sema reduced at this
//! instantiation to the object type JavaScript's spread builds. Each part is a place of a
//! concrete object type (a spread source, or a temporary holding a run of explicit properties);
//! each field of the result is shared from the last part that has a field of that name, and an
//! optional field (which sema allows only from one part) keeps that part's presence. A class's
//! optional field is always present (an own property holding `undefined` until set).

use velt_sema::hir::{self, LitValue, TyId, TyKind};

use super::{ice, unit, FnLower};
use crate::vir::{BlockId, Operand, Place, Rvalue, Ty};

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
            let sft = self.cx.member_ty(*sty, *k);
            let same = self.vty(sft) == vt;
            let widen = !same
                && matches!(self.cx.kind(fty), TyKind::Option(p, _) if self.vty(p) == self.vty(sft));
            // `a?: F` (without a presence flag) over an earlier `a: F`: its `undefined` is
            // absent, and the earlier value stays; a class's optional field is an own property
            // even while unset, so its `undefined` is copied (a `TypeError` below).
            let unwrap = !same && !widen;
            let own = unwrap && slot.is_none() && self.cx.is_class(*sty);
            let next = (!last && (slot.is_some() || (unwrap && !own))).then(|| self.new_block());
            if let Some(next) = next.filter(|_| slot.is_some()) {
                let then = self.new_block();
                self.branch(present.clone(), then, next);
                self.switch_to(then);
            }
            let mut shared = false;
            let value = if same {
                Operand::Copy(fp)
            } else if widen {
                // `a: F` under a later `a?: F`: the field holds `F | undefined`.
                shared = true;
                let v = self.share_value(Operand::Copy(fp), sft);
                match vt {
                    Ty::Agg(a) => {
                        let t = self.temp(vt);
                        let some = vec![FnLower::ctrue(), v];
                        self.assign(Place::local(t), Rvalue::Aggregate(a, some));
                        Operand::Copy(Place::local(t))
                    }
                    _ => v,
                }
            } else {
                // `a?: F` over a required `a: F`: a present `undefined` can't be kept.
                let ok = self.option_is_some(&fp, sft);
                let good = self.new_block();
                match next.filter(|_| slot.is_none()) {
                    Some(next) => self.branch(ok, good, next),
                    None => {
                        let bad = self.new_block();
                        self.branch(ok, good, bad);
                        self.switch_to(bad);
                        self.panic_msg(&format!(
                            "TypeError: a spread copies `undefined` into the required field `{name}`"
                        ));
                        self.terminate(crate::vir::Terminator::Unreachable);
                    }
                }
                self.switch_to(good);
                Operand::Copy(self.some_payload(&fp, sft))
            };
            let v = match shared {
                true => value,
                false => self.share_value(value, fty),
            };
            self.assign(Place::local(out), Rvalue::Use(v));
            if let Some(f) = flag {
                let p = if same { present } else { FnLower::ctrue() };
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

    /// The fields `object[key]` reads or writes at this instantiation: one per string literal
    /// the key's type holds (a union of literals is switched on, sema checks that their fields
    /// have one type), with the value of the key's tag that selects each.
    fn key_fields(&mut self, object: &hir::Expr, key: &hir::Expr) -> (TyId, Vec<(i128, u32)>) {
        let oty = self.sub(object.ty);
        let kty = self.sub(key.ty);
        let TyKind::Adt(d, _) = self.cx.kind(oty) else {
            ice(format_args!("a key access of {oty:?}"))
        };
        let names: Vec<(i128, TyId)> = match self.cx.kind(kty) {
            TyKind::Literal(_) => vec![(0, kty)],
            TyKind::Adt(e, _) if matches!(self.cx.hir.def(e), hir::Def::Enum(_)) => {
                let c_like = self.cx.is_c_like_enum(e);
                let n = self.cx.enum_def(e).variants.len() as u32;
                (0..n)
                    .map(|v| {
                        let at = match c_like {
                            true => self.cx.enum_def(e).variants[v as usize].discriminant as i128,
                            false => v as i128,
                        };
                        let lit = self.cx.variant_tys(kty, v).first().copied().unwrap_or(kty);
                        (at, lit)
                    })
                    .collect()
            }
            _ => ice(format_args!("a key access by {kty:?}")),
        };
        let fields = names
            .into_iter()
            .map(|(at, lit)| {
                let TyKind::Literal(LitValue::Str(name)) = self.cx.kind(lit) else {
                    ice(format_args!("a key access by {lit:?}"))
                };
                let Some(k) = (self.cx.adt_def(d).fields.iter()).position(|f| f.name == name)
                else {
                    ice(format_args!("a key access of the missing field `{name}`"))
                };
                (at, k as u32)
            })
            .collect();
        (oty, fields)
    }

    /// Enter one block per field of `fields` (`key_fields`), switching on the tag of `key`
    /// when there are several; `join` is where each continues. Returns the blocks.
    fn key_switch(&mut self, key: &hir::Expr, fields: &[(i128, u32)]) -> Vec<BlockId> {
        let blocks: Vec<BlockId> = fields.iter().map(|_| self.new_block()).collect();
        if let [only] = blocks[..] {
            self.goto(only);
            return blocks;
        }
        let kty = self.sub(key.ty);
        let kp = self.place_expr(key);
        let TyKind::Adt(e, _) = self.cx.kind(kty) else {
            ice("a key switch on a non-union")
        };
        let value = match self.cx.is_c_like_enum(e) {
            true => Operand::Copy(kp),
            false => Operand::Copy(super::operand::proj(&kp, crate::vir::Proj::Field(0))),
        };
        let default = self.new_block();
        let cases = fields.iter().zip(&blocks).map(|(f, b)| (f.0, *b)).collect();
        self.terminate(crate::vir::Terminator::Switch {
            value,
            cases,
            default,
        });
        self.switch_to(default);
        self.terminate(crate::vir::Terminator::Unreachable);
        blocks
    }

    /// `Intrinsic::KeyRead`: `object[key]`, the field shared, as `object.<key>` reads it.
    pub(super) fn key_read(&mut self, object: &hir::Expr, key: &hir::Expr, ty: TyId) -> Operand {
        let (oty, fields) = self.key_fields(object, key);
        let fty = self.sub(ty);
        self.expr(key);
        let place = self.place_expr(object);
        if self.cx.is_unit(fty) {
            return unit();
        }
        let vt = self.vty(fty);
        let out = self.temp(vt);
        let join = self.new_block();
        let blocks = self.key_switch(key, &fields);
        for ((_, k), b) in fields.iter().zip(blocks) {
            self.switch_to(b);
            let fp = self.field_place(&place, oty, *k);
            let v = self.share_value(Operand::Copy(fp), fty);
            self.assign(Place::local(out), Rvalue::Use(v));
            self.goto(join);
        }
        self.switch_to(join);
        // An owned temporary like any call's result: a consumer takes it, else it is dropped.
        self.own_temp(out, fty);
        Operand::Copy(Place::local(out))
    }

    /// `Intrinsic::KeyWrite`: `object[key] = value`, as `object.<key> = value` writes it.
    pub(super) fn key_write(
        &mut self,
        object: &hir::Expr,
        key: &hir::Expr,
        value: &hir::Expr,
    ) -> Operand {
        let (_, fields) = self.key_fields(object, key);
        self.expr(key);
        let join = self.new_block();
        let blocks = self.key_switch(key, &fields);
        for ((_, k), b) in fields.iter().zip(blocks) {
            self.switch_to(b);
            let field = hir::Expr {
                kind: hir::ExprKind::Field {
                    base: Box::new(object.clone()),
                    index: *k,
                    mode: hir::UseMode::BorrowMut,
                },
                ty: value.ty,
                span: value.span,
            };
            self.assign_expr(&field, value);
            self.goto(join);
        }
        self.switch_to(join);
        unit()
    }
}
