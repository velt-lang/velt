//! `JSON.parse<T>(s)` (docs/std/encoding/json.md): compile-time generated per-type glue over
//! the rt JSON pull reader (rt_abi_async.md §12). One function per type, shared by every use
//! (which also handles recursive types). `JSON.stringify` is written by a source generator in
//! Velt (velt/encoding/json/generate), not here.
//!
//! - parse: `Glue::JsonParse` of the document type owns the reader, checks for trailing input and
//!   turns a failure into the owned `JsonError.message`; `Glue::JsonRead` decodes one value
//!   (read.rs, object.rs). The call site throws the prelude's `JsonError { message }`.
//!
//! Decoders share a failure context `{ expected: string, path: string }`: the failing decoder
//! stores what it expected (a static string), and every level prepends its path segment
//! (`.name`, `[3]`) while returning, so paths cost nothing on success.

mod db_bind;
mod literal;
mod map;
mod object;
mod read;
mod reader;
mod rows_read;
mod union;
mod union_object;

pub(in crate::lower) use map::RecordOrder;

use velt_sema::hir::{self, AdtKind, TyId, TyKind};

use super::rt::Rt;
use super::{cint, ice, unit, Cx, FnLower, Glue};
use crate::vir::{AggId, BinOp, BlockId, Operand, Place, Proj, Rvalue, Ty, STR_AGG};

/// Qualified name of the prelude's dynamic JSON value class.
const PRELUDE_JSON_VALUE: &str = velt_common::std_item!(prelude_json, "JsonValue");

const STR: Ty = Ty::Agg(STR_AGG);
/// `velt_rt_json_reader_new_with`'s flag: the source is a `u8[]` of binary rows (rt_abi.md).
const FLAG_ROWS: i128 = 4;
/// `velt_rt_json_reader_new_with`'s flag for a driver's live rows (a `RawCursor`).
const FLAG_CURSOR: i128 = 8;

/// What `JSON.parse`'s glue reads.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::lower) enum JsonSource {
    /// JSON text (a string).
    Text,
    /// A driver's binary rows (a `u8[]`).
    Rows,
    /// A driver's live rows (the address of its cursor, a `u64`).
    Cursor,
}

/// `velt_rt_json_reader_peek` kinds (rt_abi_async.md §12.3).
const TOKEN_NULL: u32 = 1;
const TOKEN_TRUE: u32 = 2;
const TOKEN_FALSE: u32 = 3;
const TOKEN_NUMBER: u32 = 4;
const TOKEN_STRING: u32 = 5;

/// `s` as a JSON string literal (for messages).
fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A path segment prepended to the failure path while returning from a failed decoder.
enum Seg<'a> {
    /// `.name` of a known field.
    Field(&'a str),
    /// `.<key>` of an unknown key (pointer to the key string).
    Key(Operand),
    /// `[i]` (u64 operand).
    Index(Operand),
}

/// The name of the prelude's `JsonError` class.
const PRELUDE_JSON_ERROR: &str = velt_common::std_item!(prelude_json, "JsonError");

impl Cx<'_> {
    /// `{ expected: string, path: string }`, the decoders' failure context.
    fn json_ctx_agg(&mut self) -> AggId {
        if let Some(a) = self.lay.json_ctx {
            return a;
        }
        let a = self.new_agg("json ctx".into(), &[STR, STR]);
        self.lay.json_ctx = Some(a);
        a
    }

    /// The prelude's `JsonError` type (a struct or class with a `message: string` field).
    fn json_error_ty(&mut self) -> TyId {
        let d = self
            .json_error_def()
            .unwrap_or_else(|| ice("JSON.parse needs the prelude's `JsonError` type"));
        self.intern(TyKind::Adt(d, vec![]))
    }

    /// The prelude's `JsonError` definition: `std/prelude/json/json::JsonError`, else (a program
    /// built without the real prelude, as in unit tests) the first type named `JsonError`. A
    /// user's own class of that name is never it.
    pub(super) fn json_error_def(&self) -> Option<hir::DefId> {
        let named = |f: &dyn Fn(&str) -> bool| {
            self.hir
                .defs
                .iter()
                .position(|d| matches!(d, hir::Def::Adt(a) if f(&a.name)))
        };
        named(&|n| n == PRELUDE_JSON_ERROR)
            .or_else(|| {
                if self
                    .hir
                    .defs
                    .iter()
                    .any(|d| matches!(d, hir::Def::Adt(a) if a.name.starts_with(velt_common::std_dir!(prelude))))
                {
                    return None;
                }
                named(&|n| {
                    n == "JsonError" || n.ends_with("::JsonError") || n.ends_with(".JsonError")
                })
            })
            .map(|d| hir::DefId(d as u32))
    }

    /// Is `t` the prelude's `JsonValue` type (written by its `__writeJson`, decoded by its
    /// `__fromReader`)? Matched by its exact qualified name, so a user class named `JsonValue`
    /// (even in a module named `json`) stays an ordinary class.
    pub(in crate::lower) fn is_json_value(&self, t: TyId) -> bool {
        match self.types.kind(t) {
            TyKind::Adt(d, _) => match self.hir.def(*d) {
                hir::Def::Adt(a) => a.name == PRELUDE_JSON_VALUE,
                _ => false,
            },
            _ => false,
        }
    }
}

impl FnLower<'_, '_> {
    /// `JSON.parse<T>(a)` with reader options `flags` / `depth` (`i64`s): the decoded value,
    /// or a thrown `JsonError`.
    pub(super) fn json_parse(
        &mut self,
        a: &hir::Expr,
        flags: &hir::Expr,
        depth: &hir::Expr,
        source: JsonSource,
        ty: TyId,
    ) -> Operand {
        let t = self.sub(ty);
        let a_ty = self.sub(a.ty);
        let v = self.expr(a);
        let flags = self.expr(flags);
        let depth = self.expr(depth);
        if self.dead() {
            return unit();
        }
        let flags = self.cast_to(flags, Ty::I64, Ty::U32);
        let depth = self.cast_to(depth, Ty::I64, Ty::U32);
        // `u8[]` rows and a cursor: the reader reads them as rows (`FLAG_ROWS`, `FLAG_CURSOR`),
        // never as text.
        let flags = match source {
            JsonSource::Text => flags,
            JsonSource::Rows | JsonSource::Cursor => {
                let flag = if source == JsonSource::Rows {
                    FLAG_ROWS
                } else {
                    FLAG_CURSOR
                };
                let f = Rvalue::Binary(BinOp::BitOr, flags, cint(flag, Ty::U32));
                self.rvalue_temp(Ty::U32, f)
            }
        };
        // A zero-sized target (`null`, a literal type) is only checked: decode into a dummy.
        let vt = self.cx.ty(t);
        let zero_sized = vt == Ty::Unit;
        let src = match source {
            JsonSource::Rows => {
                // The array itself, also when the operand only refers to it (a borrowed
                // parameter).
                let avt = self.cx.ty(a_ty);
                let p = self.operand_place(v, avt);
                let p = self.content(&p, a_ty);
                self.addr(p)
            }
            // The cursor's address.
            JsonSource::Cursor => self.rvalue_temp(Ty::Ptr, Rvalue::Cast(v, Ty::Ptr)),
            JsonSource::Text => self.operand_addr(v, STR),
        };
        let out = self.temp(if zero_sized { Ty::U8 } else { vt });
        if !zero_sized {
            self.json_init(out, t);
        }
        let err = self.temp(STR);
        let oa = self.addr(Place::local(out));
        let ea = self.addr(Place::local(err));
        // Rows of plain objects from a cursor: read by column (rows_read.rs).
        let ok = if source == JsonSource::Cursor && self.rows_read_supported(t) {
            self.call_glue(Glue::RowsRead, t, vec![src, oa, ea])
        } else {
            self.call_glue(Glue::JsonParse, t, vec![src, flags, depth, oa, ea])
        };
        let (err_bb, ok_bb) = (self.new_block(), self.new_block());
        self.branch(ok, ok_bb, err_bb);
        self.switch_to(err_bb);
        let e = self.json_error_value(Operand::Copy(Place::local(err)));
        self.record_throw_loc();
        let et = self.cx.json_error_ty();
        self.route_error(e, et);
        self.switch_to(ok_bb);
        if zero_sized {
            return unit();
        }
        self.owned_result(Some(out), t)
    }

    /// A `JsonError` with the owned `message`, named `SyntaxError` (as the prelude's
    /// constructor names it); any other field is zero.
    fn json_error_value(&mut self, message: Operand) -> Operand {
        let et = self.cx.json_error_ty();
        let TyKind::Adt(d, _) = self.cx.kind(et) else {
            ice("JsonError is not a struct or class")
        };
        let adt = self.cx.adt_def(d);
        let msg = adt.fields.iter().position(|f| f.name == "message");
        let msg = msg.unwrap_or_else(|| ice("JsonError has no `message` field")) as u32;
        let name_field = adt.fields.iter().position(|f| f.name == "#name");
        if adt.kind == AdtKind::Class {
            let obj = self.alloc_object_raw(et);
            let p = self.field_place(&obj, et, msg);
            self.store(p, message);
            if let Some(n) = name_field {
                let name = self.str_lit("SyntaxError");
                let p = self.field_place(&obj, et, n as u32);
                self.store(p, name);
            }
            return Operand::Copy(obj);
        }
        let Ty::Agg(a) = self.cx.ty(et) else {
            ice("JsonError layout")
        };
        let tys = self.cx.adt_field_tys(et);
        let mut ops = vec![];
        for (i, ft) in tys.into_iter().enumerate() {
            let vt = self.cx.ty(ft);
            if i as u32 == msg {
                ops.push(message.clone());
            } else if vt != Ty::Unit {
                ops.push(self.zero_value(vt));
            }
        }
        self.rvalue_temp(Ty::Agg(a), Rvalue::Aggregate(a, ops))
    }

    /// `ctx.path = <segment> + ctx.path` (the context is at pointer local `ctx`).
    fn json_prepend(&mut self, ctx: crate::vir::Local, seg: Seg) {
        let ca = self.cx.json_ctx_agg();
        let path = Place {
            local: ctx,
            proj: vec![Proj::Deref(Ty::Agg(ca)), Proj::Field(1)],
        };
        let nb = self.temp(STR);
        let b = self.addr(Place::local(nb));
        self.call_rt(Rt::StrbufNew, vec![cint(0, Ty::U64), b.clone()], None);
        match seg {
            Seg::Field(name) => self.push_text(&b, &format!(".{name}")),
            Seg::Key(k) => {
                self.push_text(&b, ".");
                self.call_rt(Rt::StrbufPushStr, vec![b.clone(), k], None);
            }
            Seg::Index(i) => {
                self.push_text(&b, "[");
                self.call_rt(Rt::StrbufPushU64, vec![b.clone(), i], None);
                self.push_text(&b, "]");
            }
        }
        let pa = self.addr(path);
        self.call_rt(Rt::StrbufPushStr, vec![b.clone(), pa.clone()], None);
        self.call_rt(Rt::StrDrop, vec![pa.clone()], None);
        self.call_rt(Rt::StrbufFinish, vec![b, pa], None);
    }

    /// `ctx.expected = expected; goto to`.
    fn json_fail(&mut self, ctx: crate::vir::Local, expected: &str, to: BlockId) {
        let ca = self.cx.json_ctx_agg();
        let e = Place {
            local: ctx,
            proj: vec![Proj::Deref(Ty::Agg(ca)), Proj::Field(0)],
        };
        let lit = self.str_lit(expected);
        self.assign(e, Rvalue::Use(lit));
        self.goto(to);
    }
}
