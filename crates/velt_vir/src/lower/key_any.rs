//! `Intrinsic::KeyRead` / `KeyWrite` (spread_object.rs) at an instantiation whose object type is
//! `any`: `s[k]` in `function get<T extends object, K extends keyof T>(s: T, k: K)` called with
//! a proxy (`new Proxy(...)` is an `any`) or another `any`. The access is the one `o[k]` on an
//! `any` makes (any_member.rs, any_set_site.rs), so a proxy's `get` and `set` traps run, as in
//! JavaScript.

use velt_sema::hir::{self, TyId, TyKind};

use super::FnLower;
use crate::vir::{Operand, Place, Rvalue, Ty};

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
        let ka = self.key_name_addr(key);
        let vt = self.cx.ty(fty);
        if self.dead() {
            return self.zero_value(vt);
        }
        if !self.cx.is_dynamic(fty) {
            super::ice(format_args!("a key read of `any` as {fty:?}"));
        }
        let out = self.temp(vt);
        let oa = self.addr(Place::local(out));
        self.any_get_str(&data, &desc, ka, oa);
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
        let ka = self.key_name_addr(key);
        self.any_set_named(&data, &desc, ka, value)
    }

    /// The address of the string the key `key` holds: a `string` key's own, else the literal
    /// its tag selects (`key_names`).
    fn key_name_addr(&mut self, key: &hir::Expr) -> Operand {
        let kty = self.sub(key.ty);
        let kv = self.expr(key);
        if matches!(self.cx.kind(kty), TyKind::Str) {
            let kp = self.place_of(kv, kty);
            return self.addr(kp);
        }
        let names = self.key_names(key);
        let slot = Place::local(self.temp(Ty::Ptr));
        let join = self.new_block();
        let blocks = self.key_switch(key, &names);
        for ((_, name), b) in names.iter().zip(blocks) {
            self.switch_to(b);
            let a = self.proxy_key_addr(name);
            self.assign(slot.clone(), Rvalue::Use(a));
            self.goto(join);
        }
        self.switch_to(join);
        Operand::Copy(slot)
    }
}
