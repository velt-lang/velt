//! `Intrinsic::SpreadObject` (#350; hir_encodings.md "Spreads in generic bodies"): an object
//! literal that spreads a value whose fields are known only per instantiation (`{ ...t, $set }`
//! with `t: T`). Its type is the deferred `TyOp::Spread`, which sema reduced at this
//! instantiation to the object type JavaScript's spread builds. Each part is a place of a
//! concrete object type (a spread source, or a temporary holding a run of explicit properties);
//! each field of the result is shared from the last part that has a field of that name, and an
//! optional field (which sema allows only from one part) keeps that part's presence. A class's
//! optional field is always present (an own property holding `undefined` until set).

use velt_sema::hir::{self, LitValue, Nothing, TyId, TyKind};

use super::operand::proj;
use super::{ice, unit, FnLower};
use crate::vir::{BlockId, Operand, Place, Proj, Rvalue, Terminator, Ty};

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
        let TyKind::Adt(d, _) = self.cx.kind(oty) else {
            ice(format_args!("a key access of {oty:?}"))
        };
        let fields = (self.key_names(key).into_iter())
            .map(|(at, name)| {
                let Some(k) = (self.cx.adt_def(d).fields.iter()).position(|f| f.name == name)
                else {
                    ice(format_args!("a key access of the missing field `{name}`"))
                };
                (at, k as u32)
            })
            .collect();
        (oty, fields)
    }

    /// The property names the key `key` of `object[key]` may hold at this instantiation: one
    /// per string literal of its type, with the value of the key's tag that selects each.
    pub(super) fn key_names(&mut self, key: &hir::Expr) -> Vec<(i128, String)> {
        let kty = self.sub(key.ty);
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
        names
            .into_iter()
            .map(|(at, lit)| match self.cx.kind(lit) {
                TyKind::Literal(LitValue::Str(name)) => (at, name),
                _ => ice(format_args!("a key access by {lit:?}")),
            })
            .collect()
    }

    /// Enter one block per field of `fields` (`key_fields`), switching on the tag of `key`
    /// when there are several; `join` is where each continues. Returns the blocks.
    pub(super) fn key_switch<F>(&mut self, key: &hir::Expr, fields: &[(i128, F)]) -> Vec<BlockId> {
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
        let oty = self.sub(object.ty);
        if self.cx.is_dynamic(oty) {
            return self.key_read_any(object, key, ty);
        }
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
        let ftys = self.cx.adt_field_tys(oty);
        let blocks = self.key_switch(key, &fields);
        for ((_, k), b) in fields.iter().zip(blocks) {
            self.switch_to(b);
            let fi = ftys[*k as usize];
            // A proxiable object may be a proxy: its `get` trap answers.
            let fp = match self.cx.proxiable(oty) {
                true => {
                    let hop = self.retained_hop(&place, oty);
                    let ptr = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(hop)));
                    self.proxy_read_at(&ptr, oty, *k)
                }
                false => self.field_place(&place, oty, *k),
            };
            let v = self.share_value(Operand::Copy(fp), fi);
            // Keys whose fields have different types: the field as the union `T[K]`.
            let v = self.key_value_into(v, fi, fty);
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
        let oty = self.sub(object.ty);
        if self.cx.is_dynamic(oty) {
            return self.key_write_any(object, key, value);
        }
        let (oty, fields) = self.key_fields(object, key);
        let vty = self.sub(value.ty);
        let ftys = self.cx.adt_field_tys(oty);
        if fields.iter().any(|(_, k)| ftys[*k as usize] != vty) {
            return self.key_write_checked(object, key, value, (oty, vty), &fields);
        }
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

    /// `object[key] = value` where the keys' fields have different types and `value` is their
    /// union (`T[K]`): the value is evaluated once, and the field the key names takes the
    /// value's member, checked: a member of another type stops the program with a `TypeError`,
    /// where TypeScript would store a value of the wrong type
    /// (docs/internals/design/deferred-types.md, decision 4).
    fn key_write_checked(
        &mut self,
        object: &hir::Expr,
        key: &hir::Expr,
        value: &hir::Expr,
        (oty, vty): (TyId, TyId),
        fields: &[(i128, u32)],
    ) -> Operand {
        // Owned: each field's branch stores the member it holds (`key_read`'s result is a
        // registered temporary, taken over here; a place is shared).
        let v = self.consume(value);
        let vt = self.vty(vty);
        let tmp = self.temp(vt);
        self.assign(Place::local(tmp), Rvalue::Use(v));
        self.expr(key);
        let place = self.place_expr(object);
        let ftys = self.cx.adt_field_tys(oty);
        let names: Vec<String> = match self.cx.kind(oty) {
            TyKind::Adt(d, _) => (self.cx.adt_def(d).fields.iter())
                .map(|f| f.name.clone())
                .collect(),
            _ => vec![],
        };
        let join = self.new_block();
        let blocks = self.key_switch(key, fields);
        for ((_, k), b) in fields.iter().zip(blocks) {
            self.switch_to(b);
            let fi = ftys[*k as usize];
            let proxiable = self.cx.proxiable(oty);
            let fp = (!proxiable).then(|| self.field_place(&place, oty, *k));
            let name = names.get(*k as usize).cloned().unwrap_or_default();
            // The field as the `TypeError`s name it, with its type as written.
            let field = format!("`{name}` ({})", self.field_type_name(fi));
            let stored = self.key_store_value(&Place::local(tmp), vty, fi, &field);
            let Some(fp) = fp else {
                // A proxy's `set` trap takes the value (`proxy_write`).
                let target = hir::Expr {
                    kind: hir::ExprKind::Field {
                        base: Box::new(object.clone()),
                        index: *k,
                        mode: hir::UseMode::BorrowMut,
                    },
                    ty: fi,
                    span: value.span,
                };
                self.proxy_store(&place, oty, *k, &target, stored);
                self.goto(join);
                continue;
            };
            if !self.cx.is_unit(fi) {
                self.drop_glue(fp.clone(), fi);
                self.assign(fp.clone(), Rvalue::Use(stored));
            }
            // A write makes an optional field present (`Cx::presence_slot`).
            if let Some(slot) = self.cx.presence_slot(oty, *k) {
                let mut flag = fp.clone();
                if let Some(Proj::Field(f)) = flag.proj.last_mut() {
                    *f = slot;
                    let t = Operand::Const(crate::vir::Const::Bool(true), Ty::Bool);
                    self.assign(flag, Rvalue::Use(t));
                }
            }
            self.goto(join);
        }
        self.switch_to(join);
        unit()
    }

    /// The owned value `v` of a field of type `from` as `to`, the type of a read through keys
    /// whose fields have different types (`T[K]`): a member is tagged with its variant, a
    /// required field's value is the option's value, and a nothing stays that nothing (`null`
    /// of a `number | null` field is the `null` of a `T[K]` that also holds `undefined`). A
    /// `T | null | undefined` is an option of a `T | null` (`Cx::kind`), taken one option at
    /// a time.
    fn key_value_into(&mut self, v: Operand, from: TyId, to: TyId) -> Operand {
        if from == to || self.dead() {
            return v;
        }
        if let TyKind::Option(u, n) = self.cx.kind(from) {
            let vt = self.vty(to);
            let out = Place::local(self.temp(vt));
            let pv = self.place_of(v, from);
            let some = self.option_is_some(&pv, from);
            let (yes, no, join) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(some, yes, no);
            self.switch_to(yes);
            let payload = Operand::Copy(self.some_payload(&pv, from));
            let w = self.key_value_into(payload, u, to);
            self.assign(out.clone(), Rvalue::Use(w));
            self.goto(join);
            self.switch_to(no);
            let none = match self.nothing_in(n, to) {
                Some(w) => w,
                None => ice("a key read of a nothing its union cannot hold"),
            };
            self.assign(out.clone(), Rvalue::Use(none));
            self.goto(join);
            self.switch_to(join);
            return Operand::Copy(out);
        }
        if let TyKind::Option(u, _) = self.cx.kind(to) {
            let w = self.key_value_into(v, from, u);
            return self.some_of(to, w);
        }
        if self.cx.is_union(to) {
            return self.widen_error(v, from, to);
        }
        v
    }

    /// `Some(v)` of option type `opt` (not registered as a temporary).
    pub(super) fn some_of(&mut self, opt: TyId, v: Operand) -> Operand {
        match self.cx.ty(opt) {
            Ty::Ptr => v,
            Ty::Bool => Operand::Const(crate::vir::Const::Bool(true), Ty::Bool),
            Ty::Agg(a) => self.rvalue_temp(
                Ty::Agg(a),
                Rvalue::Aggregate(
                    a,
                    vec![Operand::Const(crate::vir::Const::Bool(true), Ty::Bool), v],
                ),
            ),
            t => ice(format_args!("an option of type {t:?}")),
        }
    }

    /// [`Self::key_member`] into a field whose type is union `to`: each member of the value the
    /// field holds becomes that member of `to` (`"b"` of `f64 | "a" | "b"` into `"a" | "b"`).
    fn key_union_member(&mut self, src: &Place, from: TyId, to: TyId, name: &str) -> Operand {
        let dt = self.cx.ty(to);
        let dst = self.temp(dt);
        let join = self.new_block();
        let member = |lw: &mut Self, payload: Operand, m: TyId| match lw.member_variant(to, m) {
            Some(k) if lw.cx.variant_tys(to, k)[0] == m => {
                let w = lw.union_value(to, k, payload, m);
                lw.assign(Place::local(dst), Rvalue::Use(w));
                lw.goto(join);
            }
            _ => {
                lw.panic_msg(&format!(
                    "TypeError: `x[k] = v` stores a value of another type in the field {name}"
                ));
                lw.terminate(Terminator::Unreachable);
            }
        };
        if self.cx.is_union(from) {
            self.for_each_member(src, from, member);
        } else {
            member(self, Operand::Copy(src.clone()), from);
        }
        self.switch_to(join);
        Operand::Copy(Place::local(dst))
    }

    /// The value at `src` (of type `from`, the union `T[K]` or an option of it, owned) as the
    /// field type `to` of the field `name` names (`` `s` (string | null) ``): a nothing is the
    /// field's same nothing, a member of the field's type is its payload, and a member of
    /// another type, or a nothing the field cannot hold, stops the program with a `TypeError`
    /// (decision 4). A `T | null | undefined` is an option of a `T | null` (`Cx::kind`), taken
    /// one option at a time.
    fn key_store_value(&mut self, src: &Place, from: TyId, to: TyId, name: &str) -> Operand {
        if from == to {
            return Operand::Copy(src.clone());
        }
        let TyKind::Option(u, n) = self.cx.kind(from) else {
            if let TyKind::Option(g, _) = self.cx.kind(to) {
                let w = self.key_store_value(src, from, g, name);
                return self.some_of(to, w);
            }
            return self.key_member(src, from, to, name);
        };
        let vt = self.vty(to);
        let out = Place::local(self.temp(vt));
        let some = self.option_is_some(src, from);
        let (yes, no, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(some, yes, no);
        self.switch_to(no);
        let none = self.nothing_as(n, to, name);
        self.assign(out.clone(), Rvalue::Use(none));
        self.goto(join);
        self.switch_to(yes);
        let payload = self.some_payload(src, from);
        let w = self.key_store_value(&payload, u, to, name);
        self.assign(out.clone(), Rvalue::Use(w));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(out)
    }

    /// Field type `t` as the `TypeError`s of key writes name it (`number | null`).
    fn field_type_name(&self, t: TyId) -> String {
        match self.cx.kind(t) {
            TyKind::Option(g, n) => {
                let what = if n.has_null() { "null" } else { "undefined" };
                format!("{} | {what}", self.field_type_name(g))
            }
            _ => self.cx.type_name(t),
        }
    }

    /// The nothing `n` (`null` or `undefined`) as a value of field type `to`: its own nothing,
    /// or its payload's (`null` into a `T | null | undefined`), else a `TypeError`.
    fn nothing_as(&mut self, n: Nothing, to: TyId, name: &str) -> Operand {
        if let Some(w) = self.nothing_in(n, to) {
            return w;
        }
        let what = if n.has_null() { "null" } else { "undefined" };
        self.panic_msg(&format!(
            "TypeError: `x[k] = v` stores `{what}` in the field {name}"
        ));
        self.terminate(Terminator::Unreachable);
        unit()
    }

    /// The nothing `n` as a value of type `to`, if `to` holds it: its own nothing, or its
    /// payload's (`null` of a `T | null | undefined` is `Some(None)`).
    fn nothing_in(&mut self, n: Nothing, to: TyId) -> Option<Operand> {
        let TyKind::Option(g, m) = self.cx.kind(to) else {
            return None;
        };
        if m == n {
            return Some(self.none_value(to));
        }
        if matches!(self.cx.kind(g), TyKind::Option(_, k) if k == n) {
            let w = self.none_value(g);
            return Some(self.some_of(to, w));
        }
        None
    }

    /// The member of the union value at `src` (type `from`) that has type `to`, or a
    /// `TypeError` naming the field `name`; the value itself when `from` is `to`.
    fn key_member(&mut self, src: &Place, from: TyId, to: TyId, name: &str) -> Operand {
        if from == to {
            return Operand::Copy(src.clone());
        }
        if self.cx.is_union(to) {
            return self.key_union_member(src, from, to, name);
        }
        let variant = match self.cx.is_union(from) {
            true => self.member_variant(from, to),
            false => None,
        };
        let Some(variant) = variant else {
            ice(format_args!(
                "a key write of a `{}` into a `{}` field",
                self.cx.type_name(from),
                self.cx.type_name(to)
            ))
        };
        let tag = Operand::Copy(proj(src, Proj::Field(0)));
        let (ok, bad) = (self.new_block(), self.new_block());
        self.terminate(Terminator::Switch {
            value: tag,
            cases: vec![(variant as i128, ok)],
            default: bad,
        });
        self.switch_to(bad);
        self.panic_msg(&format!(
            "TypeError: `x[k] = v` stores a value of another type in the field {name}"
        ));
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
        let view = self.cx.view(from, variant);
        let vp = proj(src, Proj::Cast(view));
        match self.cx.is_unit(to) {
            true => unit(),
            false => Operand::Copy(proj(&vp, Proj::Field(1))),
        }
    }
}
