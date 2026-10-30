//! Compiler-generated per-type functions ("glue"), built on demand through the worklist and
//! shared by every use of the type (which also makes recursive types work):
//!
//! | glue      | signature                          | meaning                                  |
//! |-----------|------------------------------------|------------------------------------------|
//! | Drop      | `(p: ptr)`                         | release the value at `p`                 |
//! | Clone     | `(src: ptr, dst: ptr)`             | deep copy `*src` into uninit `*dst`      |
//! | Share     | `(src: ptr, dst: ptr)`             | copy of a value type sharing its parts (share.rs) |
//! | Format    | `(buf: ptr, p: ptr, depth: u32)`   | append console.log text (nested style) at node's depth `depth` |
//! | Eq        | `(a: ptr, b: ptr) -> bool`         | structural equality                      |
//! | Same      | `(a: ptr, b: ptr) -> bool`         | JS `===`: objects inside by identity (same.rs) |
//! | SameValue | `(a: ptr, b: ptr) -> bool`         | `Object.is`: Same with floats by SameValue (same.rs) |
//! | KeyEq     | `(a: ptr, b: ptr) -> bool`         | Eq with floats compared by SameValueZero (eq.rs) |
//! | KeySame   | `(a: ptr, b: ptr) -> bool`         | Same with floats compared by SameValueZero: `Map` keys (same.rs) |
//! | Hash      | `(p: ptr) -> u64`                  | FxHash-style combine                     |
//! | ObjDrop   | `(obj: ptr)`                       | drop a class object's fields and free it |
//! | QueuedDrop | `(b: ptr)`                        | drop the value in heap box `b` and free it, or the boxed value `b` (drop_depth.rs) |
//! | ObjClone  | `(obj: ptr) -> ptr`                | deep copy of a class object              |
//! | ObjFormat | `(buf: ptr, obj: ptr, depth: u32)` | append `Name { field: value, … }`        |
//! | ObjDeepEq | `(a: ptr, b: ptr) -> bool`         | `toEqual` of two objects of exactly this class, by their fields (eq_dyn.rs) |
//! | DynDeepEq | `(a: ptr, b: ptr) -> bool`         | `toEqual` of the data of two interface values holding this type (eq_dyn.rs) |
//! | DynDrop/DynClone/DynFormat | as Obj*, on the data pointer of an interface value |
//! | DynShare  | `(data: ptr) -> ptr`               | data of another reference (share.rs)     |
//! | Transfer  | `(p: ptr)`                         | make `*p` safe for another thread, in place (transfer.rs) |
//! | TransferRoot | `(p: ptr)`                      | Transfer of a whole value: one copy per object reached twice (velt_rt `transfer_map`) |
//! | ObjTransfer/DynTransfer | `(obj: ptr) -> ptr`  | the same for a class object / interface data (a tagged pointer: check for many threads, many.rs) |
//! | ManyCheck | `(p: ptr)`                         | panic if `*p` holds a function value that cannot be called from several threads (many.rs) |
//! | JsonWrite | `(buf: ptr, p: ptr)`               | append `JSON.stringify(*p)` to a builder |
//! | JsonWriteKey | `(buf: ptr, p: ptr, key: ptr)`  | JsonWrite of a type that passes its key (a string) to a `toJSON(key)` (json/key.rs) |
//! | JsonRead  | `(r: ptr, out: ptr, ctx: ptr) -> bool` | decode one value (json/read.rs)      |
//! | JsonParse | `(src: ptr, flags: u32, max_depth: u32, out: ptr, err: ptr) -> bool` | whole-document `JSON.parse<T>` |
//! | Trace     | `(obj: ptr, visit: ptr, ctx: ptr)` | weak maps: visit the counted objects `obj` refers to (trace.rs) |
//! | TraceIn   | `(p: ptr, visit: ptr, ctx: ptr)`   | the same for an inline value at `p`      |
//! | WeakRetain / WeakRelease | `(word: u64)`       | retain / release a weak map value word   |
//! | WeakBoxTrace | `(box: ptr, visit: ptr, ctx: ptr)` | Trace of a boxed weak map value       |
//! | ProxyGet / ProxySet | `(proxy: ptr, key: ptr, v: ptr)` | a field of type `ty` through a typed proxy's `get` / `set` trap (proxy_typed.rs) |
//! | JsText    | `(p: ptr, buf: ptr)`               | append `String(*p)` of an array that may hold itself (template/js_list.rs) |
//! | ViewDrop  | `(p: ptr)`                         | release the array view whose header is at `p` (array_view.rs) |
//! | MirrorDrop | `(p: ptr)`                        | Drop of an array that is not a view: a view's mirror (array_view.rs) |
//!
//! Class objects in a hierarchy with a vtable are dropped/cloned/formatted/transferred through
//! their vtable (slots -1/-2/-3/-6, glue/vtable.rs), so a `Dog` held as an `Animal` releases
//! the whole `Dog`.

mod clone;
mod drop;
mod drop_chain;
mod drop_depth;
mod eq;
mod eq_cycles;
mod eq_dyn;
mod eq_numbers;
mod format;
mod format_array;
mod format_map;
mod format_object;
mod format_promise;
mod generic_slots;
mod literals;
mod many;
mod thunk;
mod trace;
mod transfer;
mod transfer_env;
mod vtable;

pub(super) use generic_slots::{GenericSlot, SlotKey};
#[cfg(test)]
pub(crate) use literals::{inspect_key, inspect_quote};
pub(super) use vtable::{VtableKey, SLOT_GENERIC_TABLE, SLOT_GENERIC_TYPE};

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
    /// `(src, dst)`: another reference to the value at `src` (`share_into`), the result glue of a
    /// shared promise (#212).
    ShareSlot,
    Format,
    Eq,
    Same,
    SameValue,
    KeyEq,
    KeySame,
    /// Vitest's `toEqual` (`Intrinsic::DeepEq`): `Eq`, but a class instance compares by its
    /// fields when it isn't the same object, and floats with SameValue (glue/eq.rs).
    DeepEq,
    /// `toEqual` of two objects of exactly class `ty` (a class table slot, eq_dyn.rs).
    ObjDeepEq,
    /// `toEqual` of the data pointers of two interface values holding a `ty` (an interface
    /// table slot, eq_dyn.rs).
    DynDeepEq,
    Hash,
    ObjDrop,
    QueuedDrop,
    ObjClone,
    ObjFormat,
    DynDrop,
    DynClone,
    DynFormat,
    DynShare,
    Transfer,
    TransferRoot,
    ObjTransfer,
    DynTransfer,
    ManyCheck,
    JsonWrite,
    JsonWriteKey,
    JsonRead,
    JsonParse,
    DbBind,
    RowsRead,
    Trace,
    TraceIn,
    WeakRetain,
    WeakRelease,
    WeakBoxTrace,
    AnyTruthy,
    AnySame,
    AnyNumber,
    AnyHash,
    AnyGet,
    AnyIndex,
    AnyCall,
    AnyApply,
    AnySet,
    AnySetIndex,
    AnyHas,
    AnyDelete,
    AnyKeys,
    AnyJson,
    /// `(data, op, index, text, value) -> u64`: reflection (any_reflect.rs).
    AnyReflect,
    ProxyGet,
    ProxySet,
    /// `(data, desc, out) -> bool`: an `any` as the interface type (any_iface.rs).
    AnyIface,
    /// `(data, desc, out) -> bool`: a number of another numeric type in an `any`, converted
    /// (any_number.rs).
    AnyNumeric,
    /// `(data, desc, out)`: as `AnyNumeric`, stopping with the `TypeError` on a value that is
    /// not a number (any_number.rs).
    AnyNumericOrError,
    /// `(desc)`, never returning: the `TypeError` of a value that is not a number, for
    /// `AnyNumericOrError` (any_number.rs).
    AnyNumberError,
    JsText,
    ViewDrop,
    MirrorDrop,
}

/// Vtable slots below 0 (interface/virtual methods use slots 0..n).
pub(super) const SLOT_DROP: i128 = -1;
pub(super) const SLOT_CLONE: i128 = -2;
pub(super) const SLOT_FORMAT: i128 = -3;
pub(super) const SLOT_SHARE: i128 = -4;
/// A class's name, as a static string object (not a function): `Uncaught <Class>` reports the
/// dynamic class of an error typed as one of its bases.
pub(in crate::lower) const SLOT_NAME: i128 = -5;
/// Transfer of the concrete value to another thread (`ObjTransfer` / `DynTransfer`).
pub(super) const SLOT_TRANSFER: i128 = -6;

impl Glue {
    fn name(self) -> &'static str {
        match self {
            Glue::Drop => "drop",
            Glue::Clone => "clone",
            Glue::Share => "share",
            Glue::ShareSlot => "shareslot",
            Glue::Format => "format",
            Glue::Eq => "eq",
            Glue::Same => "same",
            Glue::SameValue => "samevalue",
            Glue::KeyEq => "keyeq",
            Glue::KeySame => "keysame",
            Glue::DeepEq => "deepeq",
            Glue::ObjDeepEq => "objdeepeq",
            Glue::DynDeepEq => "dyndeepeq",
            Glue::Hash => "hash",
            Glue::ObjDrop => "objdrop",
            Glue::QueuedDrop => "queueddrop",
            Glue::ObjClone => "objclone",
            Glue::ObjFormat => "objformat",
            Glue::DynDrop => "dyndrop",
            Glue::DynClone => "dynclone",
            Glue::DynFormat => "dynformat",
            Glue::DynShare => "dynshare",
            Glue::Transfer => "transfer",
            Glue::TransferRoot => "transferroot",
            Glue::ObjTransfer => "objtransfer",
            Glue::DynTransfer => "dyntransfer",
            Glue::ManyCheck => "manycheck",
            Glue::JsonWrite => "jsonwrite",
            Glue::JsonWriteKey => "jsonwritekey",
            Glue::JsonRead => "jsonread",
            Glue::JsonParse => "jsonparse",
            Glue::DbBind => "dbbind",
            Glue::RowsRead => "rowsread",
            Glue::Trace => "trace",
            Glue::TraceIn => "tracein",
            Glue::WeakRetain => "weakretain",
            Glue::WeakRelease => "weakrelease",
            Glue::WeakBoxTrace => "weakboxtrace",
            Glue::AnyTruthy => "anytruthy",
            Glue::AnySame => "anysame",
            Glue::AnyNumber => "anynumber",
            Glue::AnyHash => "anyhash",
            Glue::AnyGet => "anyget",
            Glue::AnyIndex => "anyindex",
            Glue::AnyCall => "anycall",
            Glue::AnyApply => "anyapply",
            Glue::AnySet => "anyset",
            Glue::AnySetIndex => "anysetindex",
            Glue::AnyHas => "anyhas",
            Glue::AnyDelete => "anydelete",
            Glue::AnyKeys => "anykeys",
            Glue::AnyJson => "anyjson",
            Glue::AnyReflect => "anyreflect",
            Glue::ProxyGet => "proxyget",
            Glue::ProxySet => "proxyset",
            Glue::AnyIface => "anyiface",
            Glue::AnyNumeric => "anynumeric",
            Glue::AnyNumericOrError => "anynumericorerror",
            Glue::AnyNumberError => "anynumbererror",
            Glue::JsText => "jstext",
            Glue::ViewDrop => "viewdrop",
            Glue::MirrorDrop => "mirrordrop",
        }
    }

    /// (VIR params, VIR return type).
    fn sig(self) -> (Vec<Ty>, Ty) {
        use Ty::*;
        match self {
            Glue::Drop
            | Glue::ObjDrop
            | Glue::QueuedDrop
            | Glue::DynDrop
            | Glue::Transfer
            | Glue::TransferRoot
            | Glue::ManyCheck
            | Glue::ViewDrop
            | Glue::MirrorDrop => (vec![Ptr], Unit),
            Glue::Clone | Glue::Share | Glue::ShareSlot => (vec![Ptr, Ptr], Unit),
            Glue::ObjClone
            | Glue::DynClone
            | Glue::DynShare
            | Glue::ObjTransfer
            | Glue::DynTransfer => (vec![Ptr], Ptr),
            Glue::Format | Glue::ObjFormat | Glue::DynFormat => (vec![Ptr, Ptr, U32], Unit),
            Glue::Eq
            | Glue::Same
            | Glue::SameValue
            | Glue::KeyEq
            | Glue::KeySame
            | Glue::DeepEq
            | Glue::ObjDeepEq
            | Glue::DynDeepEq => (vec![Ptr, Ptr], Bool),
            Glue::Hash => (vec![Ptr], U64),
            Glue::JsonWrite => (vec![Ptr, Ptr], Unit),
            Glue::JsonWriteKey => (vec![Ptr, Ptr, Ptr], Unit),
            Glue::JsonRead => (vec![Ptr, Ptr, Ptr], Bool),
            Glue::JsonParse => (vec![Ptr, U32, U32, Ptr, Ptr], Bool),
            Glue::DbBind => (vec![Ptr, Ptr], Unit),
            Glue::RowsRead => (vec![Ptr, Ptr, Ptr], Bool),
            Glue::Trace | Glue::TraceIn | Glue::WeakBoxTrace => (vec![Ptr, Ptr, Ptr], Unit),
            Glue::WeakRetain | Glue::WeakRelease => (vec![U64], Unit),
            Glue::AnyTruthy => (vec![Ptr], Bool),
            Glue::AnySame => (vec![Ptr, Ptr], Bool),
            Glue::AnyNumber => (vec![Ptr], F64),
            Glue::AnyHash => (vec![Ptr], U64),
            Glue::AnyGet => (vec![Ptr, Ptr, Ptr], U8),
            Glue::AnyIndex => (vec![Ptr, F64, Ptr], U8),
            Glue::AnyCall => (vec![Ptr, Ptr, Ptr, U64, Ptr], U8),
            Glue::AnyApply => (vec![Ptr, Ptr, U64, Ptr], U8),
            Glue::AnySet => (vec![Ptr, Ptr, Ptr], U8),
            Glue::AnySetIndex => (vec![Ptr, F64, Ptr], U8),
            Glue::AnyHas | Glue::AnyDelete | Glue::AnyKeys | Glue::AnyJson => (vec![Ptr, Ptr], U8),
            Glue::AnyReflect => (vec![Ptr, U64, U64, Ptr, Ptr], U64),
            Glue::ProxyGet | Glue::ProxySet => (vec![Ptr, Ptr, Ptr], Unit),
            Glue::AnyIface | Glue::AnyNumeric => (vec![Ptr, Ptr, Ptr], Bool),
            Glue::AnyNumericOrError => (vec![Ptr, Ptr, Ptr], Unit),
            Glue::AnyNumberError => (vec![Ptr], Unit),
            Glue::JsText => (vec![Ptr, Ptr], Unit),
        }
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    pub(super) fn build_glue(cx: &'c mut Cx<'h>, g: Glue, ty: TyId) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        lw.glue = true;
        let (params, ret) = g.sig();
        let args: Vec<vir::Local> = params.iter().map(|&t| lw.new_local(t, None)).collect();
        let a = |i: usize| Operand::Copy(Place::local(args[i]));
        match g {
            Glue::Drop => lw.drop_body(args[0], ty),
            Glue::Clone => lw.clone_body(args[0], args[1], ty),
            Glue::Share => lw.share_body(args[0], args[1], ty),
            Glue::ShareSlot => lw.share_slot_body(args[0], args[1], ty),
            Glue::Format => lw.format_body(a(0), args[1], a(2), ty),
            Glue::Eq => lw.eq_body(args[0], args[1], ty),
            Glue::Same => lw.same_body(args[0], args[1], ty),
            Glue::SameValue => lw.same_value_body(args[0], args[1], ty),
            Glue::KeyEq => lw.key_eq_body(args[0], args[1], ty),
            Glue::KeySame => lw.key_same_body(args[0], args[1], ty),
            Glue::DeepEq => lw.deep_eq_body(args[0], args[1], ty),
            Glue::ObjDeepEq => lw.obj_deep_eq_body(args[0], args[1], ty),
            Glue::DynDeepEq => lw.dyn_data_deep_eq_body(args[0], args[1], ty),
            Glue::Hash => lw.hash_body(args[0], ty),
            Glue::ObjDrop => lw.obj_drop_body(args[0], ty),
            Glue::QueuedDrop => lw.queued_drop_body(args[0], ty),
            Glue::ObjClone => lw.obj_clone_body(args[0], ty),
            Glue::ObjFormat => lw.obj_format_body(a(0), args[1], a(2), ty),
            Glue::DynDrop => lw.dyn_drop_body(args[0], ty),
            Glue::DynClone => lw.dyn_clone_body(args[0], ty),
            Glue::DynFormat => lw.dyn_format_body(a(0), args[1], a(2), ty),
            Glue::DynShare => lw.dyn_share_body(args[0], ty),
            Glue::Transfer => lw.transfer_body(args[0], ty),
            Glue::TransferRoot => lw.transfer_root_body(args[0], ty),
            Glue::ObjTransfer => lw.obj_transfer_body(args[0], ty),
            Glue::DynTransfer => lw.dyn_transfer_body(args[0], ty),
            Glue::ManyCheck => lw.many_check_body(args[0], ty),
            Glue::JsonWrite => lw.json_write_body(a(0), args[1], None, ty),
            Glue::JsonWriteKey => lw.json_write_body(a(0), args[1], Some(args[2]), ty),
            Glue::JsonRead => lw.json_read_body(args[0], args[1], args[2], ty),
            Glue::JsonParse => {
                lw.json_parse_body(args[0], (args[1], args[2]), args[3], args[4], ty)
            }
            Glue::DbBind => lw.db_bind_body(a(0), args[1], ty),
            Glue::RowsRead => lw.rows_read_body(a(0), args[1], args[2], ty),
            Glue::Trace => lw.trace_body(args[0], args[1], args[2], ty),
            Glue::TraceIn => lw.trace_in_body(args[0], args[1], args[2], ty),
            Glue::WeakBoxTrace => lw.weak_box_trace_body(args[0], args[1], args[2], ty),
            Glue::WeakRetain => lw.weak_retain_body(args[0], ty),
            Glue::WeakRelease => lw.weak_release_body(args[0], ty),
            Glue::AnyTruthy => lw.any_truthy_body(args[0], ty),
            Glue::AnySame => lw.any_same_body(args[0], args[1], ty),
            Glue::AnyNumber => lw.any_number_body(args[0], ty),
            Glue::AnyHash => lw.any_hash_body(args[0], ty),
            Glue::AnyGet | Glue::AnyIndex | Glue::AnyCall | Glue::AnyApply => {
                lw.any_member_glue(g, &args, ty)
            }
            Glue::AnySet | Glue::AnySetIndex => lw.any_set_glue(g, &args, ty),
            Glue::AnyHas => lw.any_has_glue(args[0], args[1], ty),
            Glue::AnyDelete => lw.any_delete_glue(args[0], args[1], ty),
            Glue::AnyKeys => lw.any_keys_glue(args[0], args[1], ty),
            Glue::AnyJson => lw.any_json_glue(args[0], args[1], ty),
            Glue::AnyReflect => lw.any_reflect_glue(&args, ty),
            Glue::ProxyGet => lw.proxy_get_glue(args[0], args[1], args[2], ty),
            Glue::ProxySet => lw.proxy_set_glue(args[0], args[1], args[2], ty),
            Glue::AnyIface => lw.any_iface_body(args[0], args[1], args[2], ty),
            Glue::AnyNumeric => lw.any_numeric_body(args[0], args[1], args[2], ty, false),
            Glue::AnyNumericOrError => lw.any_numeric_body(args[0], args[1], args[2], ty, true),
            Glue::AnyNumberError => lw.any_type_error(a(0), ty),
            Glue::JsText => lw.js_text_body(args[0], a(1), ty),
            Glue::ViewDrop => lw.view_drop_body(args[0], ty),
            Glue::MirrorDrop => {
                lw.mirror_drop = true;
                lw.drop_body(args[0], ty);
            }
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
                let f = self.cx.func_direct(Work::Glue(Glue::Clone, ty), self.mode);
                self.call(vir::Callee::Func(f), vec![s, d], None, false);
            }
        }
    }

    /// Call glue `g` for `ty` with `args`, storing a non-Unit result in a fresh temp.
    pub(super) fn call_glue(&mut self, g: Glue, ty: TyId, args: Vec<Operand>) -> Operand {
        if matches!(
            g,
            Glue::Eq
                | Glue::Same
                | Glue::SameValue
                | Glue::KeyEq
                | Glue::KeySame
                | Glue::DeepEq
                | Glue::Hash
        ) {
            self.note_fn_compared_in(ty);
        }
        let f = self.cx.func_direct(Work::Glue(g, ty), self.mode);
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
