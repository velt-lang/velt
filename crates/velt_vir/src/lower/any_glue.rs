//! The member slots of the `any` descriptor, per concrete type (any_member.rs has the slot
//! table and the statuses): fields, getters and methods of struct / class / object types by
//! name (`Program::dyn_members`), `length` and elements of arrays, `length` of strings, and
//! calls of function values. A class object seen through a base class's table is handed to
//! its own class's glue (its class id), so subclass members are found too.

use velt_sema::hir::{self, DefId, DynMember, DynMemberKind, Nothing, PassMode, TyId, TyKind};

use super::any_member::{SLOT_APPLY, ST_NOT_FN, ST_OK, ST_UNSUPPORTED};
use super::any_proxy::ProxyKey;
use super::glue::Glue;
use super::operand::proj;
use super::rt::Rt;
use super::{cint, FnLower, Work};
use crate::vir::{self, BinOp, Callee, Operand, Place, Proj, Rvalue, Terminator, Ty, STR_AGG};

/// Members every JavaScript object has through `Object.prototype`: reading one through `any`
/// must not answer `undefined`.
pub(super) const OBJECT_PROTO: [&str; 12] = [
    "constructor",
    "toString",
    "toLocaleString",
    "valueOf",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "__proto__",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
];

/// A method or getter ready to call (only a `Proxy` method throws: proxy_throws.rs).
pub(super) struct Ready {
    pub(super) def: DefId,
    pub(super) targs: Vec<TyId>,
    pub(super) ret: TyId,
    pub(super) throws: Option<TyId>,
}

impl FnLower<'_, '_> {
    pub(super) fn ret_status(&mut self, s: i128) {
        self.terminate(Terminator::Return(cint(s, Ty::U8)));
    }

    /// Does the string at address `key` equal `name`?
    pub(super) fn key_is(&mut self, key: vir::Local, name: &str) -> Operand {
        let lit = self.str_lit(name);
        let la = self.operand_addr(lit, Ty::Agg(STR_AGG));
        let out = Place::local(self.temp(Ty::U8));
        let k = Operand::Copy(Place::local(key));
        self.call_rt(Rt::StrEq, vec![k, la], Some(out.clone()));
        self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, Operand::Copy(out), cint(0, Ty::U8)),
        )
    }

    /// Branch on `key == name`: the current block becomes the matching one; returns the block
    /// to continue in when it does not match.
    pub(super) fn on_key(&mut self, key: vir::Local, name: &str) -> vir::BlockId {
        let m = self.key_is(key, name);
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(m, yes, no);
        self.switch_to(yes);
        no
    }

    /// A struct / class / object type (not an enum or union).
    pub(super) fn plain_adt(&self, ty: TyId) -> Option<(DefId, Vec<TyId>)> {
        match self.cx.kind(ty) {
            TyKind::Adt(d, args) if matches!(self.cx.hir.def(d), hir::Def::Adt(_)) => {
                Some((d, args))
            }
            _ => None,
        }
    }

    /// For a class whose objects may be of a subclass: return the result of glue `g` of the
    /// object's own class (a generic subclass: `ST_UNSUPPORTED`). Continues in the block for
    /// an object of `d` itself.
    pub(super) fn any_subclass_dispatch(
        &mut self,
        g: Glue,
        ty: TyId,
        d: DefId,
        params: &[vir::Local],
    ) {
        if !self.cx.is_class(ty) || !self.cx.has_header(d) {
            return;
        }
        let (lo, hi) = self.cx.class_range(d);
        if lo == hi {
            return;
        }
        let mut subs = vec![];
        for i in 0..self.cx.hir.defs.len() {
            let s = DefId(i as u32);
            if let hir::Def::Adt(a) = self.cx.hir.def(s) {
                if a.kind == hir::AdtKind::Class && s != d {
                    let (id, _) = self.cx.class_range(s);
                    if id > lo && id <= hi {
                        subs.push((id, s, a.generics));
                    }
                }
            }
        }
        let vt = self.obj_vtable(Operand::Copy(Place::local(params[0])), ty);
        let id = self.vtable_class_id(vt);
        let own = self.new_block();
        let blocks: Vec<_> = subs.iter().map(|_| self.new_block()).collect();
        let cases = subs
            .iter()
            .zip(&blocks)
            .map(|((cid, _, _), b)| (*cid as i128, *b))
            .collect();
        // `own` is reached by the class id of `d` itself (the cases list the subclasses only).
        self.terminate(Terminator::Switch {
            value: id,
            cases,
            default: own,
        });
        for ((_, s, generics), b) in subs.into_iter().zip(blocks) {
            self.switch_to(b);
            if generics != 0 {
                self.ret_status(ST_UNSUPPORTED);
                continue;
            }
            let sty = self.cx.intern(TyKind::Adt(s, vec![]));
            let f = self.cx.func(Work::Glue(g, sty));
            let out = Place::local(self.temp(Ty::U8));
            let args = params
                .iter()
                .map(|&p| Operand::Copy(Place::local(p)))
                .collect();
            self.call(Callee::Func(f), args, Some(out.clone()), false);
            self.terminate(Terminator::Return(Operand::Copy(out)));
        }
        self.switch_to(own);
    }

    /// The members of `d` by name (`Program::dyn_members`).
    pub(super) fn dyn_members_of(&self, d: DefId) -> Vec<DynMember> {
        self.cx
            .hir
            .dyn_members
            .as_ref()
            .and_then(|m| m.get(&d).cloned())
            .unwrap_or_default()
    }

    /// Member `m` of a value of type `ty`, when it can be called from glue.
    pub(super) fn ready(&mut self, ty: TyId, m: &DynMember) -> Option<Ready> {
        let (_, args) = self.plain_adt(ty)?;
        let owner = self.cx.subst(m.owner, &args);
        let TyKind::Adt(_, targs) = self.cx.kind(owner) else {
            return None;
        };
        let f = self.cx.fn_def(m.def);
        if f.generics as usize != targs.len() || f.is_generator {
            return None;
        }
        if f.params.first().is_none_or(|p| p.mode == PassMode::Owned) {
            return None;
        }
        let throws = self.cx.fn_throws(f, &targs).filter(|_| !f.is_async);
        (throws.is_none() || self.literal_accessor(owner, m) || self.is_proxy(owner))
            .then_some(())?;
        let (ret, _) = self.cx.call_sig(f);
        let ret = self.cx.subst(ret, &targs);
        if !self.cx.any_boxable(ret) {
            return None;
        }
        Some(Ready {
            def: m.def,
            targs,
            ret,
            throws,
        })
    }

    /// `this` for a method called on the value of type `ty` an `any`'s `data` holds.
    pub(super) fn any_this(&mut self, data: vir::Local, ty: TyId) -> Operand {
        let vp = self.any_value_place(data, ty);
        match self.cx.ty(ty) {
            Ty::Agg(_) => self.addr(vp),
            _ => Operand::Copy(vp),
        }
    }

    /// Call `r` with `this` and the owned argument values `args`; box the result into `out`.
    pub(super) fn any_invoke(
        &mut self,
        r: &Ready,
        this: Operand,
        args: Vec<(Place, TyId)>,
        out: &Place,
    ) {
        let std_result = self.cx.fn_def(r.def).name.starts_with("std/");
        let v = self.invoke_ready(r, this, args);
        self.any_box_with(v, r.ret, out, std_result);
    }

    /// Call `r` with `this` and the owned argument values `args`: its result, owned (of type
    /// `r.ret`).
    pub(super) fn invoke_ready(
        &mut self,
        r: &Ready,
        this: Operand,
        args: Vec<(Place, TyId)>,
    ) -> Operand {
        let f = self.cx.fn_def(r.def);
        let modes: Vec<PassMode> = f.params.iter().map(|p| p.mode).collect();
        // Glue has no source location: a standard-library method it calls (a proxy's `__keys`)
        // reports none either, rather than its own line in the prelude.
        let fid = match self.panic_loc() {
            None if self.cx.tracks_caller(r.def) => self.cx.func(super::Work::Tracked(
                r.def,
                r.targs.clone(),
                super::srcloc::UNKNOWN_LOC,
            )),
            _ => self.cx.func_for(r.def, r.targs.clone()),
        };
        let mut argv = vec![this];
        let mut after = vec![];
        for ((p, t), mode) in args.into_iter().zip(modes.into_iter().skip(1)) {
            match (self.cx.ty(t), mode) {
                (Ty::Unit, _) => {}
                (Ty::Agg(_), PassMode::Owned) => argv.push(self.addr(p)),
                (Ty::Agg(_), _) => {
                    argv.push(self.addr(p.clone()));
                    after.push((p, t));
                }
                (_, PassMode::Owned) => argv.push(Operand::Copy(p)),
                _ => {
                    argv.push(Operand::Copy(p.clone()));
                    after.push((p, t));
                }
            }
        }
        let v = self.finish_ready_call(Callee::Func(fid), argv, r);
        for (p, t) in after {
            self.drop_glue(p, t);
        }
        if let Operand::Copy(p) = &v {
            self.take_temp(&p.clone());
        }
        v
    }

    /// The `any` argument `i` (a `u64` operand) of the buffer `argv`.
    pub(super) fn any_arg_place(&mut self, argv: vir::Local, i: Operand) -> Place {
        let any = self.cx.any_ty();
        let vt = self.cx.ty(any);
        let (size, _) = self.cx.size_align(vt);
        let off = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, i, cint(size as i128, Ty::U64)),
        );
        let base = Operand::Copy(Place::local(argv));
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, base, off));
        let pp = self.operand_place(p, Ty::Ptr);
        proj(&pp, Proj::Deref(vt))
    }

    /// Drop the arguments `from..argc` of `argv`.
    pub(super) fn any_drop_args(&mut self, argv: vir::Local, from: u64, argc: vir::Local) {
        let any = self.cx.any_ty();
        let i = Place::local(self.temp(Ty::U64));
        self.assign(i.clone(), Rvalue::Use(cint(from as i128, Ty::U64)));
        let (head, body, exit) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(head);
        self.switch_to(head);
        let n = Operand::Copy(Place::local(argc));
        let more = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Lt, Operand::Copy(i.clone()), n),
        );
        self.branch(more, body, exit);
        self.switch_to(body);
        let ap = self.any_arg_place(argv, Operand::Copy(i.clone()));
        self.drop_glue(ap, any);
        let next = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Add, Operand::Copy(i.clone()), cint(1, Ty::U64)),
        );
        self.assign(i, Rvalue::Use(next));
        self.goto(head);
        self.switch_to(exit);
    }

    /// The arguments converted to `params` (owned temporaries): argument `k` checked as by
    /// `AnyTo`, a missing one `undefined` (`null` for a nullable parameter, a `TypeError` for
    /// another); extra arguments are dropped.
    fn any_params(
        &mut self,
        argv: vir::Local,
        argc: vir::Local,
        params: &[TyId],
    ) -> Vec<(Place, TyId)> {
        let out = self.any_fixed_params(argv, argc, params);
        self.any_drop_args(argv, params.len() as u64, argc);
        out
    }

    /// [`Self::any_params`] without dropping the arguments past `params`.
    pub(super) fn any_fixed_params(
        &mut self,
        argv: vir::Local,
        argc: vir::Local,
        params: &[TyId],
    ) -> Vec<(Place, TyId)> {
        let any = self.cx.any_ty();
        let mut out = vec![];
        for (k, &p) in params.iter().enumerate() {
            let vt = self.cx.ty(p);
            // A `void` parameter (no argument passed): a stand-in place; a missing one is fine.
            let unit = vt == Ty::Unit;
            let slot = Place::local(self.temp(if unit { Ty::U8 } else { vt }));
            let has = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(
                    BinOp::Lt,
                    cint(k as i128, Ty::U64),
                    Operand::Copy(Place::local(argc)),
                ),
            );
            let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(has, yes, no);
            self.switch_to(yes);
            let ap = self.any_arg_place(argv, cint(k as i128, Ty::U64));
            let avt = self.cx.ty(any);
            let ap = Place::local(self.copy_to_temp(Operand::Copy(ap), avt));
            let v = self.any_unbox(&ap, any, p);
            if !unit {
                self.store(slot.clone(), v);
            }
            self.goto(done);
            self.switch_to(no);
            if unit {
            } else if self.cx.is_dynamic(p) {
                self.any_nothing_into(Nothing::Undefined, &slot);
            } else if matches!(self.cx.kind(p), TyKind::Option(..)) {
                let none = self.none_value(p);
                self.store(slot.clone(), none);
            } else {
                let want = self.any_target_name(p);
                let an = super::any_errors::article_of(&want);
                self.panic_msg(&format!(
                    "TypeError: the value is not {an} {want} (it is undefined)"
                ));
                self.terminate(Terminator::Unreachable);
                let dead = self.new_block();
                self.switch_to(dead);
            }
            self.goto(done);
            self.switch_to(done);
            out.push((slot, p));
        }
        out
    }

    /// Call the function value at `fp` (type `fty`) with the arguments; box the result into
    /// `out` and return the status. `field`: `fp` is a field, which the function may replace
    /// while it runs (`this.f = …`): it runs on a share of the value, dropped after the call.
    fn any_apply_value(
        &mut self,
        fp: &Place,
        fty: TyId,
        field: bool,
        argv: vir::Local,
        argc: vir::Local,
        out: &Place,
    ) {
        let TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        } = self.cx.kind(fty)
        else {
            self.any_drop_args(argv, 0, argc);
            return self.ret_status(ST_UNSUPPORTED);
        };
        if self.cx.error_ty(Some(throws)).is_some() || !self.cx.any_boxable(ret) {
            self.any_drop_args(argv, 0, argc);
            return self.ret_status(ST_UNSUPPORTED);
        }
        let slots = match (rest, params.split_last()) {
            // A rest parameter gets the arguments past the fixed ones, packed (`any_rest`).
            (true, Some((&last, fixed))) => {
                let mut slots = self.any_fixed_params(argv, argc, fixed);
                slots.push(self.any_rest_arg(argv, argc, fixed.len() as u64, last));
                slots
            }
            _ => self.any_params(argv, argc, &params),
        };
        let held = field.then(|| {
            let v = self.share_value(Operand::Copy(fp.clone()), fty);
            let vt = self.cx.ty(fty);
            Place::local(self.copy_to_temp(v, vt))
        });
        let fp = held.as_ref().unwrap_or(fp);
        let code = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(fp, Proj::Field(0)))),
        );
        let env = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(fp, Proj::Field(1)))),
        );
        let (mut av, mut pt) = (vec![env], vec![Ty::Ptr]);
        for (p, t) in &slots {
            match self.cx.ty(*t) {
                Ty::Unit => {}
                Ty::Agg(_) => {
                    av.push(self.addr(p.clone()));
                    pt.push(Ty::Ptr);
                }
                s => {
                    av.push(Operand::Copy(p.clone()));
                    pt.push(s);
                }
            }
        }
        let abi = self.cx.ret_abi(ret, None);
        if abi.out.is_some() {
            pt.push(Ty::Ptr);
        }
        let callee = Callee::Ptr {
            target: code,
            params: pt,
            ret: abi.ret,
        };
        let v = self.finish_call(callee, av, ret, None);
        for (p, t) in slots {
            self.drop_glue(p, t);
        }
        if let Some(h) = held {
            self.drop_glue(h, fty);
        }
        if let Operand::Copy(p) = &v {
            self.take_temp(&p.clone());
        }
        self.any_box_into(v, ret, out);
        self.ret_status(ST_OK);
    }

    /// The body of member glue `g` (module docs) for `ty`. The calls it makes register their
    /// results in a scope of their own; every one is moved into a box before returning.
    pub(super) fn any_member_glue(&mut self, g: Glue, args: &[vir::Local], ty: TyId) {
        self.push_scope(super::ScopeKind::Block);
        match (g, args) {
            (Glue::AnyGet, &[d, k, o]) => self.any_get_body(d, k, o, ty),
            (Glue::AnyIndex, &[d, i, o]) => self.any_index_body(d, i, o, ty),
            (Glue::AnyCall, &[d, k, v, c, o]) => self.any_call_body((d, k, v, c, o), ty),
            (Glue::AnyApply, &[d, v, c, o]) => self.any_apply_body((d, v, c, o), ty),
            _ => super::ice("member glue with the wrong parameters"),
        }
        self.scopes.pop();
    }

    fn any_get_body(&mut self, data: vir::Local, key: vir::Local, out: vir::Local, ty: TyId) {
        let outp = self.any_out(out);
        if self.is_proxy(ty) {
            return self.proxy_get_body(data, ProxyKey::Str(key), &outp, ty);
        }
        if self.prelude_record(ty).is_some() {
            return self.any_get_record(data, key, &outp, ty);
        }
        if let Some((d, _)) = self.plain_adt(ty) {
            self.any_subclass_dispatch(Glue::AnyGet, ty, d, &[data, key, out]);
            let vp = self.any_value_place(data, ty);
            let fields = self.member_fields(d);
            for (i, name) in fields.iter().enumerate() {
                if name.starts_with('#') {
                    continue;
                }
                let next = self.on_key(key, name);
                let fty = self.cx.member_ty(ty, i as u32);
                let f = &self.cx.adt_def(d).fields[i];
                // A left-out optional field `q?: T` is JavaScript's `undefined` (its type's own
                // nothing, whether absent or present).
                let optional = f.optional;
                let anon = self.cx.adt_def(d).kind == hir::AdtKind::Anon;
                if anon && matches!(self.cx.kind(fty), TyKind::Unit) {
                    self.void_field_panic(name);
                    self.ret_status(ST_UNSUPPORTED);
                } else if self.cx.any_boxable(fty) {
                    let fp = self.field_place(&vp, ty, i as u32);
                    let v = self.share_value(Operand::Copy(fp), fty);
                    self.any_box_with(v, fty, &outp, optional);
                    self.ret_status(ST_OK);
                } else if self.union_like(fty) {
                    // A literal, union or interface field (`kind: "ok"`): the value it holds.
                    let fp = self.field_place(&vp, ty, i as u32);
                    let v = self.share_value(Operand::Copy(fp), fty);
                    let any = self.cx.any_ty();
                    self.any_into(v, fty, any, &outp);
                    self.ret_status(ST_OK);
                } else {
                    self.ret_status(ST_UNSUPPORTED);
                }
                self.switch_to(next);
            }
            for m in self.dyn_members_of(d) {
                if m.kind == DynMemberKind::Setter || fields.contains(&m.name) {
                    continue;
                }
                let next = self.on_key(key, &m.name);
                match (m.kind, self.ready(ty, &m)) {
                    (DynMemberKind::Getter, Some(r)) => {
                        let this = self.any_this(data, ty);
                        self.any_invoke(&r, this, vec![], &outp);
                        self.ret_status(ST_OK);
                    }
                    // A method read as a value: in a program with proxies, the unbound method
                    // (proxy_method.rs); a bound function comes later.
                    (DynMemberKind::Method, Some(r)) if self.method_value_into(&r, &outp) => {
                        self.ret_status(ST_OK);
                    }
                    _ => self.ret_status(ST_UNSUPPORTED),
                }
                self.switch_to(next);
            }
            return self.any_get_fallback(key, &outp);
        }
        match self.cx.kind(ty) {
            TyKind::Tuple(es) => self.any_tuple_get(key, &outp, es.len()),
            TyKind::Array(_) | TyKind::Str => {
                let next = self.on_key(key, "length");
                let vp = self.any_value_place(data, ty);
                let n = match self.cx.kind(ty) {
                    TyKind::Str => self.str_len(Operand::Copy(vp)),
                    _ => {
                        let arr = self.content(&vp, ty);
                        Operand::Copy(proj(&arr, Proj::Field(1)))
                    }
                };
                let f = self.rvalue_temp(Ty::F64, Rvalue::Cast(n, Ty::F64));
                let f64_ = self.cx.intern(TyKind::Float(hir::FloatTy::F64));
                self.any_box_into(f, f64_, &outp);
                self.ret_status(ST_OK);
                self.switch_to(next);
                self.program_symbol_undefined(key, &outp);
                self.ret_status(ST_UNSUPPORTED);
            }
            _ => {
                self.program_symbol_undefined(key, &outp);
                self.ret_status(ST_UNSUPPORTED);
            }
        }
    }

    /// A key naming a symbol the program made (U+FFFF first, `symbols.rs` in sema) on a value
    /// without fields (a string, number, boolean, array, function, ...): no such property, so
    /// `undefined`, as Node reads a brand from a primitive. Continues in the block for another
    /// key.
    pub(super) fn program_symbol_undefined(&mut self, key: vir::Local, out: &Place) {
        let k = Operand::Copy(Place::local(key));
        let unit = Place::local(self.temp(Ty::I64));
        self.call_rt(
            Rt::StrCharCodeAt,
            vec![k, cint(0, Ty::I64)],
            Some(unit.clone()),
        );
        let m = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(unit), cint(0xffff, Ty::I64)),
        );
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(m, yes, no);
        self.switch_to(yes);
        self.any_nothing_into(Nothing::Undefined, out);
        self.ret_status(ST_OK);
        self.switch_to(no);
    }

    /// `r[key]` on a `Record<string, V>`: its `__get` (a missing key is `undefined`). A record
    /// has no members of its own; `Object.prototype`'s come later.
    fn any_get_record(&mut self, data: vir::Local, key: vir::Local, out: &Place, ty: TyId) {
        let (k, _) = self
            .prelude_record(ty)
            .unwrap_or_else(|| super::ice("record"));
        let get = self.plain_adt(ty).and_then(|(d, _)| {
            self.dyn_members_of(d)
                .into_iter()
                .find(|m| m.name == "__get" && m.kind == DynMemberKind::Method)
        });
        let ready = get.and_then(|m| self.ready(ty, &m));
        let Some(r) = ready.filter(|_| matches!(self.cx.kind(k), TyKind::Str)) else {
            return self.ret_status(ST_UNSUPPORTED);
        };
        for name in OBJECT_PROTO {
            let next = self.on_key(key, name);
            self.ret_status(ST_UNSUPPORTED);
            self.switch_to(next);
        }
        let kvt = self.cx.ty(k);
        let kp = self.deref_param(key, k);
        let ks = self.share_value(Operand::Copy(kp), k);
        let kslot = Place::local(self.copy_to_temp(ks, kvt));
        let this = self.any_this(data, ty);
        self.any_invoke(&r, this, vec![(kslot, k)], out);
        self.ret_status(ST_OK);
    }

    /// A key the fixed shape does not have: `undefined`, unless every object has it.
    fn any_get_fallback(&mut self, key: vir::Local, out: &Place) {
        for name in OBJECT_PROTO {
            let next = self.on_key(key, name);
            self.ret_status(ST_UNSUPPORTED);
            self.switch_to(next);
        }
        self.any_nothing_into(Nothing::Undefined, out);
        self.ret_status(ST_OK);
    }

    fn any_index_body(&mut self, data: vir::Local, i: vir::Local, out: vir::Local, ty: TyId) {
        let outp = self.any_out(out);
        if self.is_proxy(ty) {
            return self.proxy_get_body(data, ProxyKey::Num(i), &outp, ty);
        }
        match self.cx.kind(ty) {
            TyKind::Array(elem) => {
                let vp = self.any_value_place(data, ty);
                let arr = self.content(&vp, ty);
                let iv = Operand::Copy(Place::local(i));
                let idx = self.rvalue_temp(Ty::U64, Rvalue::Cast(iv.clone(), Ty::U64));
                let back = self.rvalue_temp(Ty::F64, Rvalue::Cast(idx.clone(), Ty::F64));
                let whole = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, back, iv));
                let len = Operand::Copy(proj(&arr, Proj::Field(1)));
                let inside =
                    self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, idx.clone(), len));
                let (w, yes, no) = (self.new_block(), self.new_block(), self.new_block());
                self.branch(whole, w, no);
                self.switch_to(w);
                self.branch(inside, yes, no);
                self.switch_to(yes);
                if self.cx.any_boxable(elem) {
                    let ep = self.elem_place(&arr, idx, elem);
                    let v = self.share_value(Operand::Copy(ep), elem);
                    self.any_box_into(v, elem, &outp);
                    self.ret_status(ST_OK);
                } else if self.union_like(elem) {
                    // A union element (`[1, "two"]` as `any`): the member it holds.
                    let ep = self.elem_place(&arr, idx, elem);
                    let v = self.share_value(Operand::Copy(ep), elem);
                    let any = self.cx.any_ty();
                    self.any_into(v, elem, any, &outp);
                    self.ret_status(ST_OK);
                } else {
                    self.ret_status(ST_UNSUPPORTED);
                }
                self.switch_to(no);
                self.any_nothing_into(Nothing::Undefined, &outp);
                self.ret_status(ST_OK);
            }
            TyKind::Tuple(es) => self.any_tuple_index(data, i, &outp, ty, &es),
            // A record's key "0" (later).
            TyKind::Adt(..) if self.prelude_record(ty).is_some() => self.ret_status(ST_UNSUPPORTED),
            TyKind::Adt(..) if self.plain_adt(ty).is_some() => {
                // No field of a fixed shape is named by a number.
                self.any_nothing_into(Nothing::Undefined, &outp);
                self.ret_status(ST_OK);
            }
            _ => self.ret_status(ST_UNSUPPORTED),
        }
    }

    fn any_call_body(
        &mut self,
        (data, key, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local, vir::Local),
        ty: TyId,
    ) {
        if self.is_proxy(ty) {
            return self.proxy_call_body((data, key, argv, argc, out), ty);
        }
        if let Some(prefix) = self.builtin_prefix(ty) {
            return self.any_builtin_call(prefix, (data, key, argv, argc, out), ty);
        }
        if self.prelude_record(ty).is_some() {
            // The prelude's `__anyRec_` functions (std/record/reflect.vlt: what
            // `Reflect` does to a record's symbol keys); a record's function values come later.
            return self.any_record_call((data, key, argv, argc, out), ty);
        }
        let outp = self.any_out(out);
        // A record's function values (later).
        let adt = self
            .plain_adt(ty)
            .filter(|_| self.prelude_record(ty).is_none());
        let Some((d, _)) = adt else {
            self.any_drop_args(argv, 0, argc);
            return self.ret_status(ST_UNSUPPORTED);
        };
        self.any_subclass_dispatch(Glue::AnyCall, ty, d, &[data, key, argv, argc, out]);
        self.any_class_call((data, key, argv, argc), (ty, d), &outp);
        let vp = self.any_value_place(data, ty);
        let fields = self.member_fields(d);
        for (i, name) in fields.iter().enumerate() {
            if name.starts_with('#') {
                continue;
            }
            let next = self.on_key(key, name);
            let fty = self.cx.member_ty(ty, i as u32);
            if matches!(self.cx.kind(fty), TyKind::FnPtr { .. }) {
                let fp = self.field_place(&vp, ty, i as u32);
                self.any_apply_value(&fp, fty, true, argv, argc, &outp);
            } else if self.cx.is_dynamic(fty) {
                // An `any` field (`f: any`, holding a function or not): called through its own
                // apply slot, on a share that keeps it alive while it runs (it may replace the
                // field). The slot owns the arguments and writes `*out`.
                let fp = self.field_place(&vp, ty, i as u32);
                let v = self.share_value(Operand::Copy(fp), fty);
                let vt = self.cx.ty(fty);
                let held = Place::local(self.copy_to_temp(v, vt));
                let (fd, fdesc) = self.any_parts(Operand::Copy(held.clone()), fty);
                let a = [argv, argc, out].map(|l| Operand::Copy(Place::local(l)));
                let a = vec![fd, a[0].clone(), a[1].clone(), a[2].clone()];
                let p = vec![Ty::Ptr, Ty::Ptr, Ty::U64, Ty::Ptr];
                let st = self.any_slot(fdesc, SLOT_APPLY, a, p);
                self.drop_glue(held, fty);
                self.terminate(Terminator::Return(st));
            } else {
                self.any_drop_args(argv, 0, argc);
                let st = if self.cx.any_boxable(fty) {
                    ST_NOT_FN
                } else {
                    ST_UNSUPPORTED
                };
                self.ret_status(st);
            }
            self.switch_to(next);
        }
        for m in self.dyn_members_of(d) {
            if m.kind == DynMemberKind::Setter || fields.contains(&m.name) {
                continue;
            }
            let next = self.on_key(key, &m.name);
            match (m.kind, self.ready(ty, &m)) {
                (DynMemberKind::Method, Some(r)) => {
                    let f = self.cx.fn_def(r.def);
                    let params: Vec<TyId> = f.params[1..]
                        .iter()
                        .map(|p| p.ty)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .map(|t| self.cx.subst(t, &r.targs))
                        .collect();
                    let args = self.any_params(argv, argc, &params);
                    let this = self.any_this(data, ty);
                    self.any_invoke(&r, this, args, &outp);
                    self.ret_status(ST_OK);
                }
                _ => {
                    self.any_drop_args(argv, 0, argc);
                    self.ret_status(ST_UNSUPPORTED);
                }
            }
            self.switch_to(next);
        }
        for name in OBJECT_PROTO {
            let next = self.on_key(key, name);
            self.any_drop_args(argv, 0, argc);
            self.ret_status(ST_UNSUPPORTED);
            self.switch_to(next);
        }
        self.any_drop_args(argv, 0, argc);
        self.ret_status(ST_NOT_FN);
    }

    fn any_apply_body(
        &mut self,
        (data, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local),
        ty: TyId,
    ) {
        let outp = self.any_out(out);
        match self.cx.kind(ty) {
            TyKind::FnPtr { .. } => {
                let fp = self.any_value_place(data, ty);
                self.any_apply_value(&fp, ty, false, argv, argc, &outp);
            }
            TyKind::Closure(_) => {
                self.any_drop_args(argv, 0, argc);
                self.ret_status(ST_UNSUPPORTED);
            }
            // An array method (`Reflect.get(array, "push")`) runs only when called on a proxy.
            TyKind::Adt(..) if self.cx.is_array_method(ty) => {
                self.any_drop_args(argv, 0, argc);
                self.ret_status(ST_UNSUPPORTED);
            }
            _ => {
                self.any_drop_args(argv, 0, argc);
                self.ret_status(ST_NOT_FN);
            }
        }
    }
}
