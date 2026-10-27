//! `Intrinsic::KeyRead` / `KeyWrite` (spread_object.rs) at an instantiation whose object type is
//! `any`: `s[k]` in `function get<T extends object, K extends keyof T>(s: T, k: K)` called with
//! a proxy (`new Proxy(...)` is an `any`) or another `any`. The access is the one `o[k]` on an
//! `any` makes (any_member.rs, any_set_site.rs), so a proxy's `get` and `set` traps run, as in
//! JavaScript: a string key (or a literal a union's tag selects) names a member, a number key an
//! element, and a symbol key reaches a proxy's trap itself.

use velt_sema::hir::{self, LitValue, TyId, TyKind};

use super::any_proxy::ProxyKey;
use super::any_set_site::SetKey;
use super::FnLower;
use crate::vir::{self, Const, Operand, Place, Rvalue, Terminator, Ty};

/// The key of a generic key access on `any`, evaluated.
enum AnyKey {
    /// The address of a string.
    Str(Operand),
    /// An `f64`.
    Num(Operand),
    /// A local holding a symbol.
    Sym(vir::Local),
    /// A key type this access does not take (a union of strings and numbers, say).
    Other,
}

/// A symbol key on a value that is not a proxy (where the access has no member to name).
const SYM_PLAIN: &str = "TypeError: a symbol key in a generic key access of a value that is not a proxy is not supported yet (#924)";

impl FnLower<'_, '_> {
    /// `object[key]` with `object` an `any`: the member the key names, read through `any`.
    pub(super) fn key_read_any(
        &mut self,
        object: &hir::Expr,
        key: &hir::Expr,
        ty: TyId,
    ) -> Operand {
        let fty = self.sub(ty);
        let (data, desc) = self.any_receiver(object);
        let k = self.any_key(key);
        let vt = self.cx.ty(fty);
        if self.dead() {
            return self.zero_value(vt);
        }
        if !self.cx.is_dynamic(fty) {
            super::ice(format_args!("a key read of `any` as {fty:?}"));
        }
        let out = self.temp(vt);
        let oa = self.addr(Place::local(out));
        match k {
            AnyKey::Str(a) => self.any_get_str(&data, &desc, a, oa),
            AnyKey::Num(n) => self.any_index_at(&data, &desc, n, oa),
            AnyKey::Sym(l) => {
                let any = self.cx.any_ty();
                let outp = Place::local(out);
                self.key_sym_proxy(&data, &desc, |lw, obj, pty| {
                    let k = lw.proxy_key(ProxyKey::Sym(l));
                    let r = lw.proxy_receiver(obj, pty);
                    lw.proxy_invoke("__get", obj, pty, vec![(k, any), (r, any)], &outp);
                });
            }
            AnyKey::Other => self.key_any_stop(KEY_OTHER),
        }
        self.own_value(Operand::Copy(Place::local(out)), fty)
    }

    /// `object[key] = value` with `object` an `any`: the write through `any`.
    pub(super) fn key_write_any(
        &mut self,
        object: &hir::Expr,
        key: &hir::Expr,
        value: &hir::Expr,
    ) -> Operand {
        let (data, desc) = self.any_receiver(object);
        match self.any_key(key) {
            AnyKey::Str(a) => self.any_set_named(&data, &desc, SetKey::Str(a), value),
            AnyKey::Num(n) => self.any_set_named(&data, &desc, SetKey::Num(n), value),
            AnyKey::Sym(l) => {
                let vt = self.sub(value.ty);
                let vv = self.consume(value);
                if self.dead() {
                    return super::unit();
                }
                let avt = self.cx.ty(vt);
                let buf = Place::local(self.temp(avt));
                self.assign(buf.clone(), Rvalue::Use(vv));
                let any = self.cx.any_ty();
                self.key_sym_proxy(&data, &desc, |lw, obj, pty| {
                    let k = lw.proxy_key(ProxyKey::Sym(l));
                    let v = Place::local(lw.copy_to_temp(Operand::Copy(buf.clone()), avt));
                    let r = lw.proxy_receiver(obj, pty);
                    let res = Place::local(lw.temp(avt));
                    let args = vec![(k, any), (v, any), (r, any)];
                    if !lw.proxy_invoke("__set", obj, pty, args, &res) {
                        return;
                    }
                    let b = lw.cx.intern(TyKind::Bool);
                    let ok = lw.any_unbox(&res, any, b);
                    let (good, bad) = (lw.new_block(), lw.new_block());
                    lw.branch(ok, good, bad);
                    lw.switch_to(bad);
                    lw.panic_msg(
                        "TypeError: 'set' on proxy: trap returned falsish for a symbol property",
                    );
                    lw.terminate(Terminator::Unreachable);
                    lw.switch_to(good);
                });
                super::unit()
            }
            AnyKey::Other => {
                self.expr(value);
                self.key_any_stop(KEY_OTHER);
                super::unit()
            }
        }
    }

    /// Stop with the `TypeError` `msg` in the current block, then continue in a block nothing
    /// reaches.
    fn key_any_stop(&mut self, msg: &str) {
        if self.dead() {
            return;
        }
        self.panic_msg(msg);
        self.terminate(Terminator::Unreachable);
        let next = self.new_block();
        self.switch_to(next);
    }

    /// A symbol key on the `any` (`data`, `desc`): `access` on the `Proxy` object (given it and
    /// its type) when the value is a proxy, else a `TypeError` (module docs).
    fn key_sym_proxy(
        &mut self,
        data: &Operand,
        desc: &Operand,
        access: impl FnOnce(&mut Self, vir::Local, TyId),
    ) {
        let (Some(pty), Some(imp)) = (self.cx.prelude_proxy_ty(), self.cx.hir.any_impl) else {
            return self.key_any_stop(SYM_PLAIN);
        };
        if self.dead() {
            return;
        }
        let table = self.vtable_addr(super::glue::VtableKey::Impl(imp, pty));
        let is = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(vir::BinOp::Eq, desc.clone(), table),
        );
        let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is, yes, no);
        self.switch_to(yes);
        let obj = self.copy_to_temp(data.clone(), Ty::Ptr);
        access(self, obj, pty);
        self.goto(done);
        self.switch_to(no);
        self.panic_msg(SYM_PLAIN);
        self.terminate(Terminator::Unreachable);
        self.switch_to(done);
    }

    /// The key `key`, evaluated: a `string` key's address, a number as an `f64`, a symbol in a
    /// local, or for a literal key type (or a union of them) the literal its tag selects.
    fn any_key(&mut self, key: &hir::Expr) -> AnyKey {
        let kty = self.sub(key.ty);
        let kv = self.expr(key);
        match self.cx.kind(kty) {
            TyKind::Str => {
                let kp = self.place_of(kv, kty);
                return AnyKey::Str(self.addr(kp));
            }
            TyKind::Float(_) | TyKind::Int(_) => {
                return AnyKey::Num(self.rvalue_temp(Ty::F64, Rvalue::Cast(kv, Ty::F64)));
            }
            TyKind::Symbol => {
                let vt = self.cx.ty(kty);
                return AnyKey::Sym(self.copy_to_temp(kv, vt));
            }
            _ => {}
        }
        let Some(lits) = self.key_literals(key) else {
            return AnyKey::Other;
        };
        let strs = lits.iter().all(|(_, l)| matches!(l, LitValue::Str(_)));
        let nums = lits.iter().all(|(_, l)| num_lit(l).is_some());
        if !strs && !nums {
            return AnyKey::Other;
        }
        let slot = Place::local(self.temp(if strs { Ty::Ptr } else { Ty::F64 }));
        let join = self.new_block();
        let blocks = self.key_switch(key, &lits);
        for ((_, lit), b) in lits.iter().zip(blocks) {
            self.switch_to(b);
            let v = match (lit, num_lit(lit)) {
                (LitValue::Str(name), _) => self.proxy_key_addr(name),
                (_, Some(n)) => Operand::Const(Const::Float(n), Ty::F64),
                _ => super::ice("a literal key of another kind"),
            };
            self.assign(slot.clone(), Rvalue::Use(v));
            self.goto(join);
        }
        self.switch_to(join);
        match strs {
            true => AnyKey::Str(Operand::Copy(slot)),
            false => AnyKey::Num(Operand::Copy(slot)),
        }
    }
}

/// A key type a generic key access through `any` doesn't take yet.
const KEY_OTHER: &str =
    "TypeError: a key of this type in a generic key access through `any` is not supported yet (#924)";

/// The number a numeric literal key holds.
fn num_lit(l: &LitValue) -> Option<f64> {
    match l {
        LitValue::Int(_, v) => Some(*v as f64),
        LitValue::Float(_, bits) => Some(f64::from_bits(*bits)),
        _ => None,
    }
}
