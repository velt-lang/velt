//! `key in x` on an `any` holding a JavaScript built-in (any_has.rs): what the built-in's
//! prototype has (Node's `Object.getOwnPropertyNames(X.prototype)`, with the well-known symbols
//! as the key names sema gives them, `[Symbol.iterator]`), an array or typed array's element
//! indexes, and the `TypeError` of a primitive (`'k' in "str"`).

use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::rt::Rt;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Rvalue, Terminator, Ty, STR_AGG};

/// `Array.prototype`, besides `Object.prototype`'s names.
pub(super) const ARRAY_PROTO: &[&str] = &[
    "length",
    "at",
    "concat",
    "copyWithin",
    "fill",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "lastIndexOf",
    "pop",
    "push",
    "reverse",
    "shift",
    "unshift",
    "slice",
    "sort",
    "splice",
    "includes",
    "indexOf",
    "join",
    "keys",
    "entries",
    "values",
    "forEach",
    "filter",
    "flat",
    "flatMap",
    "map",
    "every",
    "some",
    "reduce",
    "reduceRight",
    "toReversed",
    "toSorted",
    "toSpliced",
    "with",
    "[Symbol.iterator]",
    "[Symbol.unscopables]",
];

/// `%TypedArray%.prototype` and a kind's own prototype (`Uint8Array.prototype`).
const TYPED_ARRAY_PROTO: &[&str] = &[
    "buffer",
    "byteLength",
    "byteOffset",
    "length",
    "entries",
    "keys",
    "values",
    "at",
    "copyWithin",
    "every",
    "fill",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "forEach",
    "includes",
    "indexOf",
    "join",
    "lastIndexOf",
    "map",
    "reverse",
    "reduce",
    "reduceRight",
    "set",
    "slice",
    "some",
    "sort",
    "subarray",
    "toReversed",
    "toSorted",
    "with",
    "BYTES_PER_ELEMENT",
    "[Symbol.iterator]",
    "[Symbol.toStringTag]",
];

const MAP_PROTO: &[&str] = &[
    "get",
    "set",
    "has",
    "delete",
    "clear",
    "entries",
    "forEach",
    "keys",
    "size",
    "values",
    "getOrInsert",
    "getOrInsertComputed",
    "[Symbol.iterator]",
    "[Symbol.toStringTag]",
];

const SET_PROTO: &[&str] = &[
    "has",
    "add",
    "delete",
    "difference",
    "clear",
    "entries",
    "forEach",
    "intersection",
    "isSubsetOf",
    "isSupersetOf",
    "isDisjointFrom",
    "size",
    "symmetricDifference",
    "union",
    "values",
    "keys",
    "[Symbol.iterator]",
    "[Symbol.toStringTag]",
];

const WEAK_MAP_PROTO: &[&str] = &[
    "delete",
    "get",
    "set",
    "has",
    "getOrInsert",
    "getOrInsertComputed",
    "[Symbol.toStringTag]",
];

const WEAK_SET_PROTO: &[&str] = &["delete", "has", "add", "[Symbol.toStringTag]"];

const DATE_PROTO: &[&str] = &[
    "toDateString",
    "toTimeString",
    "toISOString",
    "toUTCString",
    "toGMTString",
    "getDate",
    "setDate",
    "getDay",
    "getFullYear",
    "setFullYear",
    "getHours",
    "setHours",
    "getMilliseconds",
    "setMilliseconds",
    "getMinutes",
    "setMinutes",
    "getMonth",
    "setMonth",
    "getSeconds",
    "setSeconds",
    "getTime",
    "setTime",
    "getTimezoneOffset",
    "getUTCDate",
    "setUTCDate",
    "getUTCDay",
    "getUTCFullYear",
    "setUTCFullYear",
    "getUTCHours",
    "setUTCHours",
    "getUTCMilliseconds",
    "setUTCMilliseconds",
    "getUTCMinutes",
    "setUTCMinutes",
    "getUTCMonth",
    "setUTCMonth",
    "getUTCSeconds",
    "setUTCSeconds",
    "getYear",
    "setYear",
    "toJSON",
    "toLocaleDateString",
    "toLocaleTimeString",
    "toTemporalInstant",
    "[Symbol.toPrimitive]",
];

/// A regular expression's own `lastIndex` and `RegExp.prototype`.
const REGEXP_PROTO: &[&str] = &[
    "lastIndex",
    "exec",
    "dotAll",
    "flags",
    "global",
    "hasIndices",
    "ignoreCase",
    "multiline",
    "source",
    "sticky",
    "unicode",
    "unicodeSets",
    "compile",
    "test",
    "[Symbol.match]",
    "[Symbol.matchAll]",
    "[Symbol.replace]",
    "[Symbol.search]",
    "[Symbol.split]",
];

/// An error's own `stack` and `message`, and `Error.prototype`.
const ERROR_PROTO: &[&str] = &["stack", "message", "name"];

pub(super) const PROMISE_PROTO: &[&str] = &["then", "catch", "finally", "[Symbol.toStringTag]"];

const ARRAY_BUFFER_PROTO: &[&str] = &[
    "byteLength",
    "slice",
    "maxByteLength",
    "resizable",
    "resize",
    "transfer",
    "transferToFixedLength",
    "detached",
    "[Symbol.toStringTag]",
];

/// What `Function.prototype` has besides `Object.prototype`'s names.
pub(super) const FN_PROTO_NAMES: &[&str] = &[
    "name",
    "length",
    "prototype",
    "call",
    "apply",
    "bind",
    "caller",
    "arguments",
    "[Symbol.hasInstance]",
];

/// The JavaScript built-in a standard library class stands for, by its name.
fn builtin_proto(short: &str) -> Option<&'static [&'static str]> {
    Some(match short {
        "Map" => MAP_PROTO,
        "Set" => SET_PROTO,
        "WeakMap" => WEAK_MAP_PROTO,
        "WeakSet" => WEAK_SET_PROTO,
        "Date" => DATE_PROTO,
        "RegExp" => REGEXP_PROTO,
        "Error" => ERROR_PROTO,
        "ArrayBuffer" => ARRAY_BUFFER_PROTO,
        "TypedArray" => TYPED_ARRAY_PROTO,
        _ => return None,
    })
}

/// How a class's objects answer `in` beyond their own fields and members: a JavaScript
/// built-in's prototype, the standard library class (or base class) standing for it.
pub(super) struct Builtin {
    pub(super) names: &'static [&'static str],
    /// A typed array: its element indexes, up to the `len` field at this index.
    pub(super) typed_len: Option<u32>,
}

impl FnLower<'_, '_> {
    /// Is `d` a standard library class (not an object type or record the library declares)?
    pub(super) fn std_class(&self, d: DefId) -> bool {
        let a = self.cx.adt_def(d);
        a.kind == hir::AdtKind::Class && a.name.starts_with("std/")
    }

    /// The JavaScript built-in `d` or its nearest standard library base class stands for.
    pub(super) fn class_builtin(&self, d: DefId) -> Option<Builtin> {
        let mut cur = Some(d);
        while let Some(c) = cur {
            let a = self.cx.adt_def(c);
            if a.name.starts_with("std/") {
                let short = a.name.rsplit("::").next().unwrap_or(&a.name);
                if let Some(names) = builtin_proto(short) {
                    let typed_len = (short == "TypedArray").then(|| {
                        let a = self.cx.adt_def(d);
                        (a.fields.iter().position(|f| f.name == "len"))
                            .unwrap_or_else(|| super::ice("TypedArray without len"))
                            as u32
                    });
                    return Some(Builtin { names, typed_len });
                }
            }
            cur = match a.base.map(|b| self.cx.kind(b)) {
                Some(TyKind::Adt(b, _)) => Some(b),
                _ => None,
            };
        }
        None
    }

    /// Answer `yes` for each of `names`; continues in the block for another key.
    pub(super) fn has_names(&mut self, key: vir::Local, names: &[&str], yes: i128) {
        for name in names {
            let next = self.on_key(key, name);
            self.ret_status(yes);
            self.switch_to(next);
        }
    }

    /// For a key that starts with a digit: answer `yes` when it is an element index below the
    /// `u64` `len` (`"0"`, not `"01"` or `"1.5"`), else `no` (no member name starts with a
    /// digit). Continues in the block for another key.
    pub(super) fn has_index_key(&mut self, key: vir::Local, len: Operand, yes: i128, no: i128) {
        let k = Operand::Copy(Place::local(key));
        let c = Place::local(self.temp(Ty::I64));
        self.call_rt(
            Rt::StrCharCodeAt,
            vec![k.clone(), cint(0, Ty::I64)],
            Some(c.clone()),
        );
        let off = Rvalue::Binary(BinOp::Sub, Operand::Copy(c), cint(b'0' as i128, Ty::I64));
        let off = self.rvalue_temp(Ty::I64, off);
        let off = self.rvalue_temp(Ty::U64, Rvalue::Cast(off, Ty::U64));
        let digit = Rvalue::Binary(BinOp::Lt, off, cint(10, Ty::U64));
        let digit = self.rvalue_temp(Ty::Bool, digit);
        let (index, other) = (self.new_block(), self.new_block());
        self.branch(digit, index, other);
        self.switch_to(index);
        let n = Place::local(self.temp(Ty::F64));
        self.call_rt(Rt::StrToNumber, vec![k.clone()], Some(n.clone()));
        let lenf = self.rvalue_temp(Ty::F64, Rvalue::Cast(len, Ty::F64));
        let lt = Rvalue::Binary(BinOp::Lt, Operand::Copy(n.clone()), lenf);
        let lt = self.rvalue_temp(Ty::Bool, lt);
        let (check, absent) = (self.new_block(), self.new_block());
        self.branch(lt, check, absent);
        // Below the length (a digit first, so not negative): the key must be the index's own
        // text.
        self.switch_to(check);
        let i = self.rvalue_temp(Ty::U64, Rvalue::Cast(Operand::Copy(n), Ty::U64));
        let s = self.temp(Ty::Agg(STR_AGG));
        let sp = self.addr(Place::local(s));
        self.call_rt(Rt::StrFromU64, vec![i, sp.clone()], None);
        let eq = Place::local(self.temp(Ty::U8));
        self.call_rt(Rt::StrEq, vec![k, sp.clone()], Some(eq.clone()));
        self.call_rt(Rt::StrDrop, vec![sp], None);
        let st = self.rvalue_temp(
            Ty::U8,
            Rvalue::Binary(BinOp::Mul, Operand::Copy(eq), cint(yes - no, Ty::U8)),
        );
        let st = self.rvalue_temp(Ty::U8, Rvalue::Binary(BinOp::Add, st, cint(no, Ty::U8)));
        self.terminate(Terminator::Return(st));
        self.switch_to(absent);
        self.ret_status(no);
        self.switch_to(other);
    }

    /// Node's `TypeError: Cannot use 'in' operator to search for 'k' in str` for a primitive of
    /// type `ty` (a string, number, boolean or symbol) at the data pointer `data`.
    pub(super) fn has_primitive_error(&mut self, data: vir::Local, key: vir::Local, ty: TyId) {
        let vp = self.any_value_place(data, ty);
        let (_, bp) = self.new_strbuf(0);
        self.push_text(&bp, "TypeError: Cannot use 'in' operator to search for '");
        self.push_str(&bp, Operand::Copy(Place::local(key)));
        self.push_text(&bp, "' in ");
        match self.cx.kind(ty) {
            TyKind::Str => {
                let a = self.addr(vp);
                self.push_str(&bp, a);
            }
            TyKind::Symbol => self.call_rt(
                Rt::StrbufPushSymbol,
                vec![bp.clone(), Operand::Copy(vp)],
                None,
            ),
            _ => self.push_scalar(&bp, Operand::Copy(vp), ty),
        }
        let text = self.temp(Ty::Agg(STR_AGG));
        let tp = self.addr(Place::local(text));
        self.call_rt(Rt::StrbufFinish, vec![bp, tp.clone()], None);
        self.call_rt(Rt::Panic, vec![tp], None);
        self.terminate(Terminator::Unreachable);
    }
}
