//! `x.k` through `any` on an array, a string, a number or a boolean (`Intrinsic::AnyGet`,
//! any_glue.rs): a key that names none of the value's properties is `undefined`, as in Node
//! (`[1, 2].type`, `"s".kind`). A member of the prototype (a method, `[Symbol.iterator]`), one
//! of `Object.prototype`'s and an element index stay `ST_UNSUPPORTED`: a method or an element
//! read through `any` comes later (#924). The tables are Node's
//! `Reflect.ownKeys(String.prototype)` and so on, without `length` and `Object.prototype`'s
//! names (`scripts/any-has-tables.mjs` checks them).

use velt_sema::hir::{Nothing, TyId, TyKind};

use super::any_glue::OBJECT_PROTO;
use super::any_has_builtin::ARRAY_PROTO;
use super::any_member::ST_UNSUPPORTED;
use super::rt::Rt;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Rvalue, Ty};

/// `String.prototype`.
const STRING_PROTO: &[&str] = &[
    "anchor",
    "at",
    "big",
    "blink",
    "bold",
    "charAt",
    "charCodeAt",
    "codePointAt",
    "concat",
    "endsWith",
    "fontcolor",
    "fontsize",
    "fixed",
    "includes",
    "indexOf",
    "isWellFormed",
    "italics",
    "lastIndexOf",
    "link",
    "localeCompare",
    "match",
    "matchAll",
    "normalize",
    "padEnd",
    "padStart",
    "repeat",
    "replace",
    "replaceAll",
    "search",
    "slice",
    "small",
    "split",
    "strike",
    "sub",
    "substr",
    "substring",
    "sup",
    "startsWith",
    "toWellFormed",
    "trim",
    "trimStart",
    "trimLeft",
    "trimEnd",
    "trimRight",
    "toLocaleLowerCase",
    "toLocaleUpperCase",
    "toLowerCase",
    "toUpperCase",
    "[Symbol.iterator]",
];

/// `Number.prototype`.
const NUMBER_PROTO: &[&str] = &["toExponential", "toFixed", "toPrecision"];

/// The names of an array, string, number or boolean of type `ty` that a read through `any`
/// does not reach yet, and whether its element indexes are among them; `None` for other types.
fn builtin_names(kind: &TyKind) -> Option<(&'static [&'static str], bool)> {
    Some(match kind {
        TyKind::Array(_) => (ARRAY_PROTO, true),
        TyKind::Str => (STRING_PROTO, true),
        TyKind::Int(_) | TyKind::Float(_) => (NUMBER_PROTO, false),
        // `Boolean.prototype` has only `Object.prototype`'s names.
        TyKind::Bool => (&[], false),
        _ => return None,
    })
}

impl FnLower<'_, '_> {
    /// For a `ty` that [`builtin_names`] knows: `ST_UNSUPPORTED` for the keys it lists (and
    /// `Object.prototype`'s, and an element index), else `undefined` into `out`. `false`, with
    /// nothing emitted, for another type.
    pub(super) fn builtin_get_rest(&mut self, key: vir::Local, out: &Place, ty: TyId) -> bool {
        let Some((names, indexed)) = builtin_names(&self.cx.kind(ty)) else {
            return false;
        };
        if indexed {
            self.digit_key_unsupported(key);
        }
        for name in names.iter().chain(OBJECT_PROTO.iter()) {
            let next = self.on_key(key, name);
            self.ret_status(ST_UNSUPPORTED);
            self.switch_to(next);
        }
        self.any_nothing_into(Nothing::Undefined, out);
        self.ret_status(super::any_member::ST_OK);
        true
    }

    /// `ST_UNSUPPORTED` for a key that starts with a digit (an element index, or no property:
    /// no member name starts with one); continues in the block for another key.
    fn digit_key_unsupported(&mut self, key: vir::Local) {
        let c = Place::local(self.temp(Ty::I64));
        self.call_rt(
            Rt::StrCharCodeAt,
            vec![Operand::Copy(Place::local(key)), cint(0, Ty::I64)],
            Some(c.clone()),
        );
        let off = Rvalue::Binary(BinOp::Sub, Operand::Copy(c), cint(b'0' as i128, Ty::I64));
        let off = self.rvalue_temp(Ty::I64, off);
        let off = self.rvalue_temp(Ty::U64, Rvalue::Cast(off, Ty::U64));
        let digit = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, off, cint(10, Ty::U64)));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(digit, yes, no);
        self.switch_to(yes);
        self.ret_status(ST_UNSUPPORTED);
        self.switch_to(no);
    }
}
