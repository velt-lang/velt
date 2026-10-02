//! Compiler-generated per-type functions ("glue"), built on demand through the worklist and
//! shared by every use of the type (which also makes recursive types work):
//!
//! | glue      | signature                          | meaning                                  |
//! |-----------|------------------------------------|------------------------------------------|
//! | Drop      | `(p: ptr)`                         | release the value at `p`                 |
//! | Clone     | `(src: ptr, dst: ptr)`             | deep copy `*src` into uninit `*dst`      |
//! | Share     | `(src: ptr, dst: ptr)`             | copy of a value type sharing its parts (share.rs) |
//! | Format    | `(buf: ptr, p: ptr)`               | append console.log text (nested style)   |
//! | Eq        | `(a: ptr, b: ptr) -> bool`         | structural equality                      |
//! | Same      | `(a: ptr, b: ptr) -> bool`         | JS `===`: objects inside by identity (same.rs) |
//! | Hash      | `(p: ptr) -> u64`                  | FxHash-style combine                     |
//! | ObjDrop   | `(obj: ptr)`                       | drop a class object's fields and free it |
//! | ObjClone  | `(obj: ptr) -> ptr`                | deep copy of a class object              |
//! | ObjFormat | `(buf: ptr, obj: ptr)`             | append `Name { field: value, … }`        |
//! | DynDrop/DynClone/DynFormat | as Obj*, on the data pointer of an interface value |
//! | DynShare  | `(data: ptr) -> ptr`               | data of another reference (share.rs)     |
//! | JsonWrite | `(buf: ptr, p: ptr)`               | append `JSON.stringify(*p)` to a builder |
//! | JsonRead  | `(r: ptr, out: ptr, ctx: ptr) -> bool` | decode one value (json/read.rs)      |
//! | JsonParse | `(src: ptr, out: ptr, err: ptr) -> bool` | whole-document `JSON.parse<T>`     |
//!
//! Class objects in a hierarchy with a vtable are dropped/cloned/formatted through their vtable
//! (slots -1/-2/-3, glue/vtable.rs), so a `Dog` held as an `Animal` releases the whole `Dog`.

mod clone;
mod drop;
mod eq;
mod format;
mod format_map;
mod literals;
mod thunk;
mod vtable;

#[cfg(test)]
pub(crate) use literals::inspect_quote;
pub(super) use vtable::VtableKey;

use velt_sema::hir::{TyId, TyKind};

use super::rt::Rt;
use super::{Cx, FnLower, Work};
use crate::vir::{self, Function, Operand, Place, Proj, Ty};

/// Kinds of per-type glue functions (see the module table).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Glue {
    Drop,
    Clone,
    Share,
    Format,
    Eq,
    Same,
    Hash,
    ObjDrop,
    ObjClone,
    ObjFormat,
    DynDrop,
    DynClone,
    DynFormat,
    DynShare,
    JsonWrite,
    JsonRead,
    JsonParse,
}

/// Vtable slots below 0 (interface/virtual methods use slots 0..n).
pub(super) const SLOT_DROP: i128 = -1;
pub(super) const SLOT_CLONE: i128 = -2;
pub(super) const SLOT_FORMAT: i128 = -3;
pub(super) const SLOT_SHARE: i128 = -4;
/// A class's name, as a static string object (not a function): `Uncaught <Class>` reports the
/// dynamic class of an error typed as one of its bases.
pub(in crate::lower) const SLOT_NAME: i128 = -5;

impl Glue {
    fn name(self) -> &'static str {
        match self {
            Glue::Drop => "drop",
            Glue::Clone => "clone",
            Glue::Share => "share",
            Glue::Format => "format",
            Glue::Eq => "eq",
            Glue::Same => "same",
            Glue::Hash => "hash",
            Glue::ObjDrop => "objdrop",
            Glue::ObjClone => "objclone",
            Glue::ObjFormat => "objformat",
            Glue::DynDrop => "dyndrop",
            Glue::DynClone => "dynclone",
            Glue::DynFormat => "dynformat",
            Glue::DynShare => "dynshare",
            Glue::JsonWrite => "jsonwrite",
            Glue::JsonRead => "jsonread",
            Glue::JsonParse => "jsonparse",
        }
    }

    /// (VIR params, VIR return type).
    fn sig(self) -> (Vec<Ty>, Ty) {
        use Ty::*;
        match self {
            Glue::Drop | Glue::ObjDrop | Glue::DynDrop => (vec![Ptr], Unit),
            Glue::Clone | Glue::Share => (vec![Ptr, Ptr], Unit),
            Glue::ObjClone | Glue::DynClone | Glue::DynShare => (vec![Ptr], Ptr),
            Glue::Format | Glue::ObjFormat | Glue::DynFormat => (vec![Ptr, Ptr], Unit),
            Glue::Eq | Glue::Same => (vec![Ptr, Ptr], Bool),
            Glue::Hash => (vec![Ptr], U64),
            Glue::JsonWrite => (vec![Ptr, Ptr], Unit),
            Glue::JsonRead | Glue::JsonParse => (vec![Ptr, Ptr, Ptr], Bool),
        }
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    pub(super) fn build_glue(cx: &'c mut Cx<'h>, g: Glue, ty: TyId) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        let (params, ret) = g.sig();
        let args: Vec<vir::Local> = params.iter().map(|&t| lw.new_local(t, None)).collect();
        let a = |i: usize| Operand::Copy(Place::local(args[i]));
        match g {
            Glue::Drop => lw.drop_body(args[0], ty),
            Glue::Clone => lw.clone_body(args[0], args[1], ty),
            Glue::Share => lw.share_body(args[0], args[1], ty),
            Glue::Format => lw.format_body(a(0), args[1], ty),
            Glue::Eq => lw.eq_body(args[0], args[1], ty),
            Glue::Same => lw.same_body(args[0], args[1], ty),
            Glue::Hash => lw.hash_body(args[0], ty),
            Glue::ObjDrop => lw.obj_drop_body(args[0], ty),
            Glue::ObjClone => lw.obj_clone_body(args[0], ty),
            Glue::ObjFormat => lw.obj_format_body(a(0), args[1], ty),
            Glue::DynDrop => lw.dyn_drop_body(args[0], ty),
            Glue::DynClone => lw.dyn_clone_body(args[0], ty),
            Glue::DynFormat => lw.dyn_format_body(a(0), args[1], ty),
            Glue::DynShare => lw.dyn_share_body(args[0], ty),
            Glue::JsonWrite => lw.json_write_body(a(0), args[1], ty),
            Glue::JsonRead => lw.json_read_body(args[0], args[1], args[2], ty),
            Glue::JsonParse => lw.json_parse_body(args[0], args[1], args[2], ty),
        }
        let sym = format!("_G{}_{}", g.name(), lw.cx.type_symbol(ty));
        lw.finish(sym, params, ret)
    }

    /// `*p` for a glue pointer param `p` to a value of concrete type `ty`.
    pub(super) fn deref_param(&mut self, p: vir::Local, ty: TyId) -> Place {
        let vt = self.cx.ty(ty);
        Place {
            local: p,
            proj: vec![Proj::Deref(vt)],
        }
    }

    /// Deep-copy the value at `src` into (uninitialized) `dst`.
    pub(super) fn clone_into(&mut self, src: Place, dst: Place, ty: TyId) {
        if !self.cx.needs_drop(ty) {
            if self.cx.ty(ty) != Ty::Unit {
                self.assign(dst, vir::Rvalue::Use(Operand::Copy(src)));
            }
            return;
        }
        let (s, d) = (self.addr(src), self.addr(dst));
        match self.cx.kind(ty) {
            TyKind::Str => self.call_rt(Rt::StrClone, vec![s, d], None),
            _ => {
                let f = self.cx.func(Work::Glue(Glue::Clone, ty));
                self.call(vir::Callee::Func(f), vec![s, d], None, false);
            }
        }
    }

    /// Call glue `g` for `ty` with `args`, storing a non-Unit result in a fresh temp.
    pub(super) fn call_glue(&mut self, g: Glue, ty: TyId, args: Vec<Operand>) -> Operand {
        let f = self.cx.func(Work::Glue(g, ty));
        match g.sig().1 {
            Ty::Unit => {
                self.call(vir::Callee::Func(f), args, None, false);
                super::unit()
            }
            r => {
                let d = self.temp(r);
                self.call(vir::Callee::Func(f), args, Some(Place::local(d)), false);
                Operand::Copy(Place::local(d))
            }
        }
    }

    /// Call a function pointer obtained from a dispatcher.
    pub(super) fn call_entry(
        &mut self,
        f: Operand,
        args: Vec<Operand>,
        params: Vec<Ty>,
        ret: Ty,
    ) -> Operand {
        let callee = vir::Callee::Ptr {
            target: f,
            params,
            ret,
        };
        match ret {
            Ty::Unit => {
                self.call(callee, args, None, false);
                super::unit()
            }
            r => {
                let d = self.temp(r);
                self.call(callee, args, Some(Place::local(d)), false);
                Operand::Copy(Place::local(d))
            }
        }
    }

    /// Branch: continue in a fresh block when `cond` holds, else jump to `skip`.
    pub(super) fn when(&mut self, cond: Operand, skip: vir::BlockId) {
        let then = self.new_block();
        self.branch(cond, then, skip);
        self.switch_to(then);
    }

    /// `ptr != null` as a Bool operand.
    pub(super) fn non_null(&mut self, p: Operand) -> Operand {
        self.rvalue_temp(
            Ty::Bool,
            vir::Rvalue::Binary(vir::BinOp::Ne, p, super::cint(0, Ty::Ptr)),
        )
    }
}
