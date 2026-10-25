//! Member reads and calls on `any` (phase 1b of #924; hir_encodings.md "any and unknown"):
//! `Intrinsic::AnyGet`, `AnyIndex`, `AnyCall` and `AnyApply`. Each is one indirect call
//! through a descriptor slot of the value held (any_glue.rs builds the slots per concrete
//! type), so every dynamic member access goes through this single entry point:
//!
//! | slot | entry |
//! |---|---|
//! | 5 | `AnyGet`: `(data, key: *string, out: *any) -> u8` |
//! | 6 | `AnyIndex`: `(data, i: f64, out: *any) -> u8` |
//! | 7 | `AnyCall`: `(data, key: *string, argv: *any, argc: u64, out: *any) -> u8` |
//! | 8 | `AnyApply`: `(data, argv: *any, argc: u64, out: *any) -> u8` |
//! | 9, 10 | `AnySet`, `AnySetIndex` (any_set.rs; only in a program that writes through `any`) |
//! | 11 | `AnyHas` (any_has.rs; only in a program that tests `key in x` on an `any`) |
//! | 15 | `AnyDelete` (any_delete.rs; only in a program that deletes through `any`) |
//!
//! The arguments `argv[0..argc]` are owned by the slot (converted to the parameter types or
//! dropped). The status is `ST_OK` (`*out` written), `ST_UNSUPPORTED` (a member this phase
//! cannot reach yet) or `ST_NOT_FN` (the member is not a function); the caller turns the last
//! two into a `TypeError`. The slots exist only when `Program::dyn_members` is set.

use velt_sema::hir::{self, Nothing, TyId, TyKind};

use super::any::{SLOT_NUMBER, TAG_NULL, TAG_NUMBER, TAG_STRING, TAG_UNDEFINED};
use super::glue::{Glue, VtableKey};
use super::operand::proj;
use super::rt::Rt;
use super::{cint, ice, Cx, FnLower, Work};
use crate::vir::{
    self, BinOp, Callee, Const, Operand, Place, Proj, Rvalue, Terminator, Ty, STR_AGG,
};

pub(super) const SLOT_GET: i128 = 5;
pub(super) const SLOT_INDEX: i128 = 6;
pub(super) const SLOT_CALL: i128 = 7;
pub(super) const SLOT_APPLY: i128 = 8;

pub(super) const ST_OK: i128 = 0;
pub(super) const ST_UNSUPPORTED: i128 = 1;
pub(super) const ST_NOT_FN: i128 = 2;

impl Cx<'_> {
    /// The type `any`.
    pub(in crate::lower) fn any_ty(&mut self) -> TyId {
        let i = self
            .hir
            .any_impl
            .unwrap_or_else(|| ice("`any` without its implementation"));
        let iface = self.hir.impls[i as usize].iface;
        self.intern(TyKind::Dyn(iface, vec![]))
    }

    /// The member slots of the `any` table of `ty` (module docs), when the program has them.
    pub(super) fn any_member_entries(&mut self, ty: TyId) -> Vec<(i128, Const)> {
        if self.hir.dyn_members.is_none() {
            return vec![];
        }
        let mut slots = vec![
            (SLOT_GET, Glue::AnyGet),
            (SLOT_INDEX, Glue::AnyIndex),
            (SLOT_CALL, Glue::AnyCall),
            (SLOT_APPLY, Glue::AnyApply),
        ];
        // The write slots only when the program writes through `any` (from the pass before).
        if self.boxing.any_writes {
            slots.push((super::any_set::SLOT_SET, Glue::AnySet));
            slots.push((super::any_set::SLOT_SET_INDEX, Glue::AnySetIndex));
        }
        if self.boxing.any_has {
            slots.push((super::any_has::SLOT_HAS, Glue::AnyHas));
        }
        if self.boxing.any_delete {
            slots.push((super::any_delete::SLOT_DELETE, Glue::AnyDelete));
        }
        slots
            .into_iter()
            .map(|(s, g)| (s, Const::Func(self.func(Work::Glue(g, ty)))))
            .collect()
    }

    /// Can glue box a value of `t` (`any_box_into`)? Types sema unpacks before boxing (unions,
    /// literal and enum types) and interface values are not reached yet.
    pub(super) fn any_boxable(&self, t: TyId) -> bool {
        if self.is_dynamic(t) {
            return true;
        }
        match self.types.kind(t) {
            TyKind::Unit => true,
            TyKind::Option(inner, _) => self.is_dynamic(*inner) || self.any_boxable_plain(*inner),
            _ => self.any_boxable_plain(t),
        }
    }

    fn any_boxable_plain(&self, t: TyId) -> bool {
        match self.types.kind(t) {
            TyKind::Bool
            | TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Str
            | TyKind::Symbol
            | TyKind::Array(_)
            | TyKind::Tuple(_)
            | TyKind::FnPtr { .. } => true,
            TyKind::Adt(d, args) => match self.hir.def(*d) {
                hir::Def::Adt(_) => true,
                // A union whose members are boxable (`string | symbol`, an element of the keys
                // `Reflect.ownKeys` lists): boxed as the member it holds.
                hir::Def::Enum(e) => {
                    e.is_union
                        && args.is_empty()
                        && e.variants.iter().all(|v| match v.payload.as_slice() {
                            [p] => !self.is_dynamic(*p) && self.any_boxable_plain(*p),
                            _ => false,
                        })
                }
                _ => false,
            },
            _ => false,
        }
    }
}

impl FnLower<'_, '_> {
    /// The `any` place `*out` for a pointer local `out`.
    pub(super) fn any_out(&mut self, out: vir::Local) -> Place {
        let any = self.cx.any_ty();
        let vt = self.cx.ty(any);
        Place {
            local: out,
            proj: vec![Proj::Deref(vt)],
        }
    }

    /// Box the owned value `v` of type `t` (`Cx::any_boxable`) into the uninitialized `any` at
    /// `out`, as `ToDyn` after sema's unpacking: an option holds its payload or the dynamic
    /// `null` / `undefined`, `void` is `undefined`.
    pub(super) fn any_box_into(&mut self, v: Operand, t: TyId, out: &Place) {
        self.any_box_with(v, t, out, false);
    }

    /// `any_box_into`; `std_result`: an absent `T | null` is JavaScript's `undefined`: the
    /// result of a standard library function (`Map.get`, `find`, as sema does) or a left-out
    /// optional field.
    pub(super) fn any_box_with(&mut self, v: Operand, t: TyId, out: &Place, std_result: bool) {
        if self.cx.is_dynamic(t) {
            self.assign(out.clone(), Rvalue::Use(v));
            return;
        }
        match self.cx.kind(t) {
            TyKind::Unit => self.any_nothing_into(Nothing::Undefined, out),
            TyKind::Option(inner, n) => {
                let vt = self.cx.ty(t);
                let p = Place::local(self.copy_to_temp(v, vt));
                let some = self.option_is_some(&p, t);
                let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
                self.branch(some, yes, no);
                self.switch_to(yes);
                let pp = self.some_payload(&p, t);
                self.any_box_into(Operand::Copy(pp), inner, out);
                self.goto(done);
                self.switch_to(no);
                let n = if std_result { Nothing::Undefined } else { n };
                self.any_nothing_into(n, out);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Adt(d, _) if matches!(self.cx.hir.def(d), hir::Def::Enum(e) if e.is_union) => {
                // A union (`Cx::any_boxable`): the member it holds.
                let vt = self.cx.ty(t);
                let p = Place::local(self.copy_to_temp(v, vt));
                self.for_each_variant(&p, t, |lw, _, parts| match parts.as_slice() {
                    [(pp, pt)] => lw.any_box_into(Operand::Copy(pp.clone()), *pt, out),
                    _ => ice(format_args!("a union member without one payload boxed")),
                });
            }
            _ => self.any_box_raw(v, t, out),
        }
    }

    /// The dynamic `null` / `undefined` into `out`.
    pub(super) fn any_nothing_into(&mut self, n: Nothing, out: &Place) {
        let b = self.cx.intern(TyKind::Bool);
        let n = if n.has_null() {
            Nothing::Null
        } else {
            Nothing::Undefined
        };
        let t = self.cx.intern(TyKind::Option(b, n));
        let none = self.none_value(t);
        self.any_box_raw(none, t, out);
    }

    fn any_box_raw(&mut self, v: Operand, t: TyId, out: &Place) {
        // An object type is one counted object when the program writes through `any`
        // (`any_set`), so the box holds the object itself, not a copy.
        let any = self.cx.any_ty();
        self.cx.note_dyn_identity(any, t);
        let data = if self.cx.is_class(t) || self.cx.boxed(t) {
            v
        } else {
            let vt = self.cx.ty(t);
            let b = self.alloc(vt);
            let bp = self.operand_place(b.clone(), Ty::Ptr);
            self.store(proj(&bp, Proj::Deref(vt)), v);
            b
        };
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let table = self.vtable_addr(VtableKey::Impl(imp, t));
        let (data, table) = match self.cx.proxiable(t) {
            true => self.proxy_any_parts(data, table),
            false => (data, table),
        };
        let (data, table) = match self.cx.array_view(t) {
            true => self.view_any_parts(data, table, t),
            false => (data, table),
        };
        self.assign(proj(out, Proj::Field(0)), Rvalue::Use(data));
        self.assign(proj(out, Proj::Field(1)), Rvalue::Use(table));
    }

    /// Call descriptor slot `slot` of `desc` with `args` of VIR types `params`.
    pub(super) fn any_slot(
        &mut self,
        desc: Operand,
        slot: i128,
        args: Vec<Operand>,
        params: Vec<Ty>,
    ) -> Operand {
        let f = self.dispatch(desc, slot);
        let out = Place::local(self.temp(Ty::U8));
        let callee = Callee::Ptr {
            target: f,
            params,
            ret: Ty::U8,
        };
        self.call(callee, args, Some(out.clone()), false);
        Operand::Copy(out)
    }

    /// The `(data, descriptor)` of the `any` expression `o`, read once. The access may run any
    /// getter or method of the value held, which may replace the field `o` is read from
    /// (`box.g.kill()` where `kill` sets `this.box.g = null`): a receiver reached through a
    /// counted object or a shared cell is shared for the statement, as `stable_borrow` does for
    /// a counted receiver, so the old value lives until the call returns.
    pub(super) fn any_receiver(&mut self, o: &hir::Expr) -> (Operand, Operand) {
        let t = self.sub(o.ty);
        let v = if !self.dead() && (self.through_counted(o, t) || self.in_shared_cell(o)) {
            let v = self.expr(o);
            let s = self.share_value(v, t);
            self.own_value(s, t)
        } else {
            self.expr(o)
        };
        let (data, desc) = self.any_parts(v, t);
        (
            self.rvalue_temp(Ty::Ptr, Rvalue::Use(data)),
            self.rvalue_temp(Ty::Ptr, Rvalue::Use(desc)),
        )
    }

    /// `TypeError: Cannot read properties of undefined (reading 'k')` (Node's text) when the
    /// descriptor `desc` holds `null` / `undefined`; `key` is the key string's address, or a
    /// number key (formatted only on that path).
    pub(super) fn any_nullish_check(&mut self, desc: &Operand, key: NullKey<'_>) {
        self.any_nullish_check_as(desc, key, false);
    }

    /// `any_nullish_check`; `setting`: for a write (`Cannot set properties of undefined
    /// (setting 'k')`).
    pub(super) fn any_nullish_check_as(&mut self, desc: &Operand, key: NullKey<'_>, setting: bool) {
        let tag = self.any_tag_of(desc.clone());
        let one = cint(TAG_NULL as i128, Ty::U64);
        let nil = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, tag.clone(), one));
        let (bad, ok) = (self.new_block(), self.new_block());
        self.branch(nil, bad, ok);
        self.switch_to(bad);
        let undef = cint(TAG_UNDEFINED as i128, Ty::U64);
        let is_undef = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, undef));
        let (u, n) = (self.new_block(), self.new_block());
        self.branch(is_undef, u, n);
        for (b, word) in [(u, "undefined"), (n, "null")] {
            self.switch_to(b);
            let (verb, ing) = if setting {
                ("set", "setting")
            } else {
                ("read", "reading")
            };
            let head = format!("TypeError: Cannot {verb} properties of {word}");
            let k = match key {
                NullKey::Str(k) => k.clone(),
                NullKey::Num(i) => {
                    let ks = self.temp(Ty::Agg(STR_AGG));
                    let ka = self.addr(Place::local(ks));
                    self.call_rt(Rt::StrFromF64, vec![i.clone(), ka.clone()], None);
                    ka
                }
            };
            self.panic_with_key(&format!("{head} ({ing} '"), &k, "')");
            self.terminate(Terminator::Unreachable);
        }
        self.switch_to(ok);
    }

    /// Stop with `before` + the string at address `key` + `after`.
    pub(super) fn panic_with_key(&mut self, before: &str, key: &Operand, after: &str) {
        let s = Ty::Agg(STR_AGG);
        let a = self.str_lit(before);
        let a = self.operand_addr(a, s);
        let ab = self.temp(s);
        let abp = self.addr(Place::local(ab));
        self.call_rt(Rt::StrConcat, vec![a, key.clone(), abp.clone()], None);
        let c = self.str_lit(after);
        let c = self.operand_addr(c, s);
        let all = self.temp(s);
        let allp = self.addr(Place::local(all));
        self.call_rt(Rt::StrConcat, vec![abp, c, allp.clone()], None);
        self.call_rt(Rt::Panic, vec![allp], None);
    }

    /// After a slot call: a `TypeError` for a status other than `ST_OK`.
    pub(super) fn any_status_check(
        &mut self,
        st: Operand,
        unsupported: (&str, Option<&Operand>, &str),
        not_fn: Option<&str>,
    ) {
        let ok = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st.clone(), cint(ST_OK, Ty::U8)),
        );
        let (good, bad) = (self.new_block(), self.new_block());
        self.branch(ok, good, bad);
        self.switch_to(bad);
        if let Some(text) = not_fn {
            let nf = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Eq, st, cint(ST_NOT_FN, Ty::U8)),
            );
            let (yes, no) = (self.new_block(), self.new_block());
            self.branch(nf, yes, no);
            self.switch_to(yes);
            self.panic_msg(&format!("TypeError: {text} is not a function"));
            self.terminate(Terminator::Unreachable);
            self.switch_to(no);
        }
        let (before, key, after) = unsupported;
        match key {
            Some(k) => self.panic_with_key(before, k, after),
            None => self.panic_msg(&format!("{before}{after}")),
        }
        self.terminate(Terminator::Unreachable);
        self.switch_to(good);
    }

    /// `AnyGet`: `o[k]` with a string key, or an `any` key read by its `typeof`.
    pub(super) fn any_get(&mut self, o: &hir::Expr, k: &hir::Expr, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let (data, desc) = self.any_receiver(o);
        let kt = self.sub(k.ty);
        let kv = self.expr(k);
        if self.dead() {
            let vt = self.cx.ty(ty);
            return self.zero_value(vt);
        }
        let avt = self.cx.ty(ty);
        let out = self.temp(avt);
        let oa = self.addr(Place::local(out));
        if self.cx.is_dynamic(kt) {
            // An `any` key: a string names a member, a number an element.
            let kp = self.place_of(kv, kt);
            let kdata = Operand::Copy(proj(&kp, Proj::Field(0)));
            let kdesc = Operand::Copy(proj(&kp, Proj::Field(1)));
            let ktag = self.any_tag_of(kdesc.clone());
            let (s_bb, n_bb, bad, done) = (
                self.new_block(),
                self.new_block(),
                self.new_block(),
                self.new_block(),
            );
            self.terminate(Terminator::Switch {
                value: ktag,
                cases: vec![(TAG_STRING as i128, s_bb), (TAG_NUMBER as i128, n_bb)],
                default: bad,
            });
            self.switch_to(s_bb);
            self.any_get_str(&data, &desc, kdata, oa.clone());
            self.goto(done);
            self.switch_to(n_bb);
            let num = self.any_key_number(&kp);
            self.any_index_at(&data, &desc, num, oa);
            self.goto(done);
            self.switch_to(bad);
            self.panic_msg(
                "TypeError: a key of this type is not supported through `any` yet (#924)",
            );
            self.terminate(Terminator::Unreachable);
            self.switch_to(done);
        } else {
            let kp = self.place_of(kv, kt);
            let ka = self.addr(kp);
            self.any_get_str(&data, &desc, ka, oa);
        }
        self.own_value(Operand::Copy(Place::local(out)), ty)
    }

    /// The number an `any` key at `kp` holds (`ToNumber`; its tag is `number`).
    pub(super) fn any_key_number(&mut self, kp: &Place) -> Operand {
        let kdata = Operand::Copy(proj(kp, Proj::Field(0)));
        let kdesc = Operand::Copy(proj(kp, Proj::Field(1)));
        let f = self.dispatch(kdesc, SLOT_NUMBER);
        let out = Place::local(self.temp(Ty::F64));
        let callee = Callee::Ptr {
            target: f,
            params: vec![Ty::Ptr],
            ret: Ty::F64,
        };
        self.call(callee, vec![kdata], Some(out.clone()), false);
        Operand::Copy(out)
    }

    pub(super) fn any_get_str(
        &mut self,
        data: &Operand,
        desc: &Operand,
        key: Operand,
        out: Operand,
    ) {
        self.any_nullish_check(desc, NullKey::Str(&key));
        let args = vec![data.clone(), key.clone(), out];
        let st = self.any_slot(desc.clone(), SLOT_GET, args, vec![Ty::Ptr; 3]);
        // In `Reflect.get`, the read a proxy forwards to its target: a method of a class
        // instance, which a method call through a proxy with traps reads, is the common case.
        let tail = if self.in_reflect {
            "' through a proxy or `Reflect.get` is not supported for this value yet, such as a method of a class instance (#823)"
        } else {
            "' through `any` is not supported for this value yet (#924)"
        };
        self.any_status_check(st, ("TypeError: reading '", Some(&key), tail), None);
    }

    fn any_index_at(&mut self, data: &Operand, desc: &Operand, i: Operand, out: Operand) {
        self.any_nullish_check(desc, NullKey::Num(&i));
        let args = vec![data.clone(), i, out];
        let st = self.any_slot(
            desc.clone(),
            SLOT_INDEX,
            args,
            vec![Ty::Ptr, Ty::F64, Ty::Ptr],
        );
        self.any_status_check(
            st,
            (
                "TypeError: indexing through `any` is not supported for this value yet (#924)",
                None,
                "",
            ),
            None,
        );
    }

    /// `AnyIndex`: `o[i]` with a number key.
    pub(super) fn any_index(&mut self, o: &hir::Expr, i: &hir::Expr, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let (data, desc) = self.any_receiver(o);
        let iv = self.expr(i);
        let iv = self.rvalue_temp(Ty::F64, Rvalue::Use(iv));
        if self.dead() {
            let vt = self.cx.ty(ty);
            return self.zero_value(vt);
        }
        let avt = self.cx.ty(ty);
        let out = self.temp(avt);
        let oa = self.addr(Place::local(out));
        self.any_index_at(&data, &desc, iv, oa);
        self.own_value(Operand::Copy(Place::local(out)), ty)
    }

    /// `AnyCall` (`key` given) and `AnyApply`: the arguments boxed into a buffer the slot owns.
    pub(super) fn any_call(
        &mut self,
        o: &hir::Expr,
        key: Option<&hir::Expr>,
        text: &hir::Expr,
        args: &[hir::Expr],
        ty: TyId,
    ) -> Operand {
        let ty = self.sub(ty);
        let (data, desc) = self.any_receiver(o);
        let ka = key.map(|k| {
            let kt = self.sub(k.ty);
            let kv = self.expr(k);
            let kp = self.place_of(kv, kt);
            self.addr(kp)
        });
        // A `null` / `undefined` receiver throws before the arguments are evaluated, as in
        // JavaScript (`n.f(g())` never calls `g`).
        if let (Some(ka), false) = (&ka, self.dead()) {
            self.any_nullish_check(&desc, NullKey::Str(ka));
        }
        // A spread call (`f(...xs)`, `o.m(a, ...xs)`): sema built the arguments as one `any[]`,
        // whose elements move into the call; its buffer is freed after it.
        let spread = match args {
            [a] => {
                let at = self.sub(a.ty);
                matches!(self.cx.kind(at), TyKind::Array(_)).then_some(at)
            }
            _ => None,
        };
        let vals = self.consume_each(args);
        if self.dead() {
            let vt = self.cx.ty(ty);
            return self.zero_value(vt);
        }
        let text = match &text.kind {
            hir::ExprKind::Lit(hir::Lit::Str(s)) => s.clone(),
            _ => "the value".to_string(),
        };
        let avt = self.cx.ty(ty);
        let mut list = None;
        let (argv, argc) = match spread {
            Some(at) => {
                let v = vals
                    .into_iter()
                    .next()
                    .expect("ICE: spread call without its array");
                let vt = self.cx.ty(at);
                let held = Place::local(self.copy_to_temp(v, vt));
                let arr = self.content(&held, at);
                let argv = Operand::Copy(proj(&arr, Proj::Field(0)));
                let argc = Operand::Copy(proj(&arr, Proj::Field(1)));
                list = Some((held, arr, at));
                (argv, argc)
            }
            None if vals.is_empty() => (cint(0, Ty::Ptr), cint(0, Ty::U64)),
            None => {
                let tup = self.cx.intern(TyKind::Tuple(vec![ty; vals.len()]));
                let tvt = self.cx.ty(tup);
                let buf = Place::local(self.temp(tvt));
                for (k, v) in vals.into_iter().enumerate() {
                    self.assign(proj(&buf, Proj::Field(k as u32)), Rvalue::Use(v));
                }
                (self.addr(buf), cint(args.len() as i128, Ty::U64))
            }
        };
        let out = self.temp(avt);
        let oa = self.addr(Place::local(out));
        let st = match &ka {
            Some(ka) => {
                let a = vec![data, ka.clone(), argv, argc, oa];
                let p = vec![Ty::Ptr, Ty::Ptr, Ty::Ptr, Ty::U64, Ty::Ptr];
                self.any_slot(desc, SLOT_CALL, a, p)
            }
            None => {
                let a = vec![data, argv, argc, oa];
                let p = vec![Ty::Ptr, Ty::Ptr, Ty::U64, Ty::Ptr];
                self.any_slot(desc, SLOT_APPLY, a, p)
            }
        };
        if let Some((held, arr, at)) = list {
            // The call owns the elements now: the array keeps none of them.
            self.assign(proj(&arr, Proj::Field(1)), Rvalue::Use(cint(0, Ty::U64)));
            self.drop_glue(held, at);
        }
        let what = "TypeError: calling '";
        let tail = "' through `any` is not supported for this value yet (#924)";
        match &ka {
            Some(k) => self.any_status_check(st, (what, Some(k), tail), Some(&text)),
            None => self.any_status_check(
                st,
                (
                    "TypeError: calling this function through `any` is not supported yet (#924)",
                    None,
                    "",
                ),
                Some(&text),
            ),
        }
        self.own_value(Operand::Copy(Place::local(out)), ty)
    }
}

/// The key a `TypeError` for a `null` / `undefined` receiver names.
#[derive(Clone, Copy)]
pub(super) enum NullKey<'a> {
    /// The address of the key string.
    Str(&'a Operand),
    /// A number key (an `f64`).
    Num(&'a Operand),
}
