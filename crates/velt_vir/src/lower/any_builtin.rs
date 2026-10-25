//! Built-in methods of strings, arrays, numbers and booleans called through `any` (#924): the
//! `AnyCall` slot of such a type (any_glue.rs) compares the key with the names of the prelude's
//! functions for its kind (std/prelude/zzzz_any_methods.vlt: `__anyStr_<name>(s, args)`,
//! `__anyArr_<name><T>(a, args)` instantiated at the element type, `__anyNum_<name>(n, args)`,
//! `__anyBool_<name>(b, args)`) and calls the one named, with the value borrowed and the
//! arguments packed into an `any[]`; its `any` result is the call's. Another name is a
//! member not supported yet (`ST_UNSUPPORTED`, a `TypeError` naming #924).

use velt_sema::hir::{self, DefId, PassMode, TyId, TyKind};

use super::any_glue::Ready;
use super::any_member::{ST_OK, ST_UNSUPPORTED};
use super::FnLower;
use crate::vir::{self, Operand, Place, Ty};

impl FnLower<'_, '_> {
    /// The name prefix of the prelude's methods for values of the concrete type `ty`, if it
    /// has built-in methods.
    pub(super) fn builtin_prefix(&self, ty: TyId) -> Option<&'static str> {
        match self.cx.kind(ty) {
            TyKind::Str => Some("__anyStr_"),
            TyKind::Array(_) => Some("__anyArr_"),
            TyKind::Int(_) | TyKind::Float(_) => Some("__anyNum_"),
            TyKind::Bool => Some("__anyBool_"),
            _ => None,
        }
    }

    /// The prelude's methods named `<prefix><name>`, by name, that take the value and an
    /// `any[]`.
    fn builtin_methods(&self, prefix: &str) -> Vec<(String, DefId)> {
        let mut out = vec![];
        for (i, d) in self.cx.hir.defs.iter().enumerate() {
            let hir::Def::Fn(f) = d else { continue };
            if !f.name.starts_with("std/prelude/") || f.params.len() != 2 {
                continue;
            }
            let Some((_, short)) = f.name.rsplit_once("::") else {
                continue;
            };
            let Some(name) = short.strip_prefix(prefix) else {
                continue;
            };
            out.push((name.to_string(), DefId(i as u32)));
        }
        out
    }

    /// The `AnyCall` slot body of a value of the concrete type `ty` with built-in methods
    /// (module docs).
    pub(super) fn any_builtin_call(
        &mut self,
        prefix: &str,
        (data, key, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local, vir::Local),
        ty: TyId,
    ) {
        let outp = self.any_out(out);
        let any = self.cx.any_ty();
        let rest = self.cx.intern(TyKind::Array(any));
        let targs = match self.cx.kind(ty) {
            TyKind::Array(e) => vec![e],
            _ => vec![],
        };
        let f64_ = self.cx.intern(TyKind::Float(hir::FloatTy::F64));
        for (name, def) in self.builtin_methods(prefix) {
            if self.cx.fn_def(def).generics as usize != targs.len() {
                continue;
            }
            let next = self.on_key(key, &name);
            let this = match self.cx.kind(ty) {
                // A number held at another width (`size` read through `any`) is a `number`.
                TyKind::Int(_) | TyKind::Float(_) if ty != f64_ => {
                    let vp = self.any_value_place(data, ty);
                    let from = self.cx.ty(ty);
                    self.cast_to(Operand::Copy(vp), from, Ty::F64)
                }
                // A method that keeps the value (passes it on as an `any`) takes a share of it.
                _ if self.cx.fn_def(def).params[0].mode == PassMode::Owned => {
                    let vp = self.any_value_place(data, ty);
                    let v = self.share_value(Operand::Copy(vp), ty);
                    let vt = self.cx.ty(ty);
                    let p = Place::local(self.copy_to_temp(v, vt));
                    self.take_temp(&p);
                    match vt {
                        Ty::Agg(_) => self.addr(p),
                        _ => Operand::Copy(p),
                    }
                }
                _ => self.any_this(data, ty),
            };
            let args = vec![self.any_rest_arg(argv, argc, 0, rest)];
            let r = Ready {
                def,
                targs: targs.clone(),
                ret: any,
            };
            self.any_invoke(&r, this, args, &outp);
            self.ret_status(ST_OK);
            self.switch_to(next);
        }
        self.any_drop_args(argv, 0, argc);
        self.ret_status(ST_UNSUPPORTED);
    }
}
