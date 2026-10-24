//! `Glue::RowsRead`: std's `JSON.parse` of a driver's row cursor (`__intrinsic_json_parse_cursor`)
//! when the row type is an object of plain fields (numbers, bools, strings, `T | null` of
//! those, `u8[]`), and the result a row or an array of rows. Instead of the JSON reader's keys,
//! each column is matched to its field once per result (velt_rt db_rows.rs) and each value read
//! by column, typed. Columns are read in order, so a row that does not fit fails where the JSON
//! decoder would, with its message; any other type is decoded by the JSON decoder.
//!
//! `(cursor, out, err) -> bool`: `out` all-zero before; on failure it is all-zero again and
//! `err` holds the message.

use velt_sema::hir::{self, IntTy, Nothing, TyId, TyKind};

use super::STR;
use crate::lower::rt::Rt;
use crate::lower::{cint, ice, FnLower};
use crate::vir::{BinOp, BlockId, Const, Local, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// Where a row that does not fit failed: what was expected, the row (-1: the single row) and
/// the field (`""`: the row itself).
#[derive(Clone, Copy)]
struct Failure {
    expected: Local,
    row: Local,
    field: Local,
}

impl FnLower<'_, '_> {
    /// Whether `JSON.parse` of a row cursor into `ty` reads the rows by column.
    pub(in crate::lower) fn rows_read_supported(&mut self, ty: TyId) -> bool {
        match self.cx.kind(ty) {
            TyKind::Array(e) => !self.cx.boxed(ty) && self.rows_row_ok(e),
            _ => self.rows_row_ok(ty),
        }
    }

    fn rows_row_ok(&mut self, ty: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return false;
        };
        if !matches!(self.cx.hir.def(d), hir::Def::Adt(_))
            || self.cx.is_union(ty)
            || self.cx.is_json_value(ty)
            || self.prelude_map(ty).is_some()
            || self.prelude_record(ty).is_some()
            || self.cx.adt_def(d).opaque
            || self.cx.boxed(ty)
        {
            return false;
        }
        let fields: Vec<(String, bool)> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.optional))
            .collect();
        let tys = self.cx.adt_field_tys(ty);
        fields.len() <= 64
            && fields
                .into_iter()
                .zip(tys)
                .enumerate()
                .all(|(i, ((name, optional), fty))| {
                    !velt_sema::property_order::is_hidden_key(&name)
                        && !optional
                        && self.cx.presence_slot(ty, i as u32).is_none()
                        && self.rows_value_ok(fty)
                })
    }

    fn rows_value_ok(&mut self, ty: TyId) -> bool {
        match self.cx.kind(ty) {
            TyKind::Option(e, Nothing::Null) => self.rows_scalar(e),
            _ => self.rows_scalar(ty) || self.rows_blob(ty),
        }
    }

    fn rows_scalar(&mut self, ty: TyId) -> bool {
        matches!(
            self.cx.kind(ty),
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str
        )
    }

    fn rows_blob(&mut self, ty: TyId) -> bool {
        !self.cx.boxed(ty)
            && matches!(self.cx.kind(ty), TyKind::Array(e) if matches!(self.cx.kind(e), TyKind::Int(IntTy::U8)))
    }

    /// `Glue::RowsRead` body.
    pub(in crate::lower) fn rows_read_body(
        &mut self,
        cursor: Operand,
        out: Local,
        err: Local,
        ty: TyId,
    ) {
        let place = self.deref_param(out, ty);
        let (row_ty, many) = match self.cx.kind(ty) {
            TyKind::Array(e) => (e, true),
            _ => (ty, false),
        };
        let table = self.rows_field_table(row_ty);
        let st = self.temp(Ty::Ptr);
        self.call_rt(Rt::RowsBegin, vec![cursor, table], Some(Place::local(st)));
        let f = Failure {
            expected: self.temp(STR),
            row: self.temp(Ty::I64),
            field: self.temp(STR),
        };
        let fail = self.new_block();
        let done = self.new_block();
        let s = Operand::Copy(Place::local(st));
        if many {
            let i = self.temp(Ty::I64);
            self.assign(Place::local(i), Rvalue::Use(cint(0, Ty::I64)));
            let (head, body) = (self.new_block(), self.new_block());
            self.goto(head);
            self.switch_to(head);
            let more = self.rt_u8(Rt::RowsNext, vec![s.clone()]);
            self.branch(more, body, done);
            self.switch_to(body);
            let et = self.cx.ty(row_ty);
            let elem = self.temp(et);
            self.json_init(elem, row_ty);
            let elem_fail = self.new_block();
            let row = Operand::Copy(Place::local(i));
            self.rows_read_row(st, &Place::local(elem), row_ty, row, f, elem_fail);
            self.push_value(&place, row_ty, Operand::Copy(Place::local(elem)));
            let n = self.rvalue_temp(
                Ty::I64,
                Rvalue::Binary(BinOp::Add, Operand::Copy(Place::local(i)), cint(1, Ty::I64)),
            );
            self.assign(Place::local(i), Rvalue::Use(n));
            self.goto(head);
            self.switch_to(elem_fail);
            self.drop_glue(Place::local(elem), row_ty);
            self.goto(fail);
        } else {
            let some = self.rt_u8(Rt::RowsNext, vec![s.clone()]);
            let (row_bb, none_bb) = (self.new_block(), self.new_block());
            self.branch(some, row_bb, none_bb);
            self.switch_to(none_bb);
            self.rows_fail(f, "object", cint(-1, Ty::I64), "", fail);
            self.switch_to(row_bb);
            self.rows_read_row(st, &place, row_ty, cint(-1, Ty::I64), f, fail);
            self.goto(done);
        }
        self.switch_to(done);
        self.call_rt(Rt::RowsEnd, vec![s.clone()], None);
        self.terminate(Terminator::Return(Operand::Const(
            Const::Bool(true),
            Ty::Bool,
        )));
        self.switch_to(fail);
        self.call_rt(Rt::RowsEnd, vec![s], None);
        self.drop_glue(place.clone(), ty);
        self.json_zero(&place, ty);
        let (ea, fa) = (
            self.addr(Place::local(f.expected)),
            self.addr(Place::local(f.field)),
        );
        let args = vec![
            ea,
            Operand::Copy(Place::local(f.row)),
            fa,
            Operand::Copy(Place::local(err)),
        ];
        self.call_rt(Rt::RowsFail, args, None);
        self.terminate(Terminator::Return(Operand::Const(
            Const::Bool(false),
            Ty::Bool,
        )));
    }

    /// The row type's field names for `velt_rt_rows_begin`: static data, a `u32` count, then per
    /// field a `u32` byte length and the name.
    fn rows_field_table(&mut self, ty: TyId) -> Operand {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            ice("rows of a non-object type")
        };
        let names: Vec<String> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let mut t = (names.len() as u32).to_le_bytes().to_vec();
        for n in &names {
            t.extend((n.len() as u32).to_le_bytes());
            t.extend(n.as_bytes());
        }
        let sid = self.cx.static_bytes(t, 4);
        Operand::Const(Const::Static(sid), Ty::Ptr)
    }

    /// Records where the row failed and goes to `fail`.
    fn rows_fail(&mut self, f: Failure, expected: &str, row: Operand, field: &str, fail: BlockId) {
        let e = self.str_lit(expected);
        self.assign(Place::local(f.expected), Rvalue::Use(e));
        self.assign(Place::local(f.row), Rvalue::Use(row));
        let n = self.str_lit(field);
        self.assign(Place::local(f.field), Rvalue::Use(n));
        self.goto(fail);
    }

    /// One row into `place` (all-zero before), column by column; on failure `place` is left
    /// for the caller to drop.
    fn rows_read_row(
        &mut self,
        st: Local,
        place: &Place,
        ty: TyId,
        row: Operand,
        f: Failure,
        fail: BlockId,
    ) {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            ice("rows of a non-object type")
        };
        if self.cx.is_class(ty) {
            let obj = self.alloc_object_raw(ty);
            self.assign(place.clone(), Rvalue::Use(Operand::Copy(obj)));
        }
        let names: Vec<String> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let tys = self.cx.adt_field_tys(ty);
        let s = Operand::Copy(Place::local(st));
        let seen = self.temp(Ty::U64);
        self.assign(Place::local(seen), Rvalue::Use(cint(0, Ty::U64)));
        let n = self.temp(Ty::U64);
        self.call_rt(Rt::RowsColumns, vec![s.clone()], Some(Place::local(n)));
        let c = self.temp(Ty::U64);
        self.assign(Place::local(c), Rvalue::Use(cint(0, Ty::U64)));
        let (head, body, next, after) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        self.goto(head);
        self.switch_to(head);
        let more = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(
                BinOp::Lt,
                Operand::Copy(Place::local(c)),
                Operand::Copy(Place::local(n)),
            ),
        );
        self.branch(more, body, after);
        self.switch_to(body);
        let fi = self.temp(Ty::I64);
        self.call_rt(
            Rt::RowsField,
            vec![s.clone(), Operand::Copy(Place::local(c))],
            Some(Place::local(fi)),
        );
        let blocks: Vec<BlockId> = names.iter().map(|_| self.new_block()).collect();
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(fi)),
            cases: blocks
                .iter()
                .enumerate()
                .map(|(i, b)| (i as i128, *b))
                .collect(),
            default: next,
        });
        let col = Operand::Copy(Place::local(c));
        for (i, b) in blocks.into_iter().enumerate() {
            self.switch_to(b);
            let (name, fty) = (&names[i], tys[i]);
            let fp = self.field_place(place, ty, i as u32);
            // A repeated column: the later value wins, as a repeated key does.
            self.drop_glue(fp.clone(), fty);
            self.json_zero(&fp, fty);
            self.rows_read_value(st, col.clone(), &fp, fty, (name, row.clone()), f, fail);
            let bit = cint(1i128 << i, Ty::U64);
            let sv = Operand::Copy(Place::local(seen));
            self.assign(Place::local(seen), Rvalue::Binary(BinOp::BitOr, sv, bit));
            self.goto(next);
        }
        self.switch_to(next);
        let c1 = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Add, Operand::Copy(Place::local(c)), cint(1, Ty::U64)),
        );
        self.assign(Place::local(c), Rvalue::Use(c1));
        self.goto(head);
        self.switch_to(after);
        // A field no column filled: only a `T | null` one stays null.
        for (i, (name, fty)) in names.iter().zip(&tys).enumerate() {
            if matches!(self.cx.kind(*fty), TyKind::Option(..)) {
                continue;
            }
            let sv = Operand::Copy(Place::local(seen));
            let bit = self.rvalue_temp(
                Ty::U64,
                Rvalue::Binary(BinOp::BitAnd, sv, cint(1i128 << i, Ty::U64)),
            );
            let has = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, bit, cint(0, Ty::U64)));
            let (ok, missing) = (self.new_block(), self.new_block());
            self.branch(has, ok, missing);
            self.switch_to(missing);
            self.rows_fail(f, &format!("field \"{name}\""), row.clone(), "", fail);
            self.switch_to(ok);
        }
    }

    /// The value of column `col` into field `fp` (all-zero before).
    #[allow(clippy::too_many_arguments)] // the column, the field and where a failure goes
    fn rows_read_value(
        &mut self,
        st: Local,
        col: Operand,
        fp: &Place,
        ty: TyId,
        (name, row): (&str, Operand),
        f: Failure,
        fail: BlockId,
    ) {
        let (target, payload_ty, nullable) = match self.cx.kind(ty) {
            TyKind::Option(e, _) => (self.some_payload(fp, ty), e, true),
            _ => (fp.clone(), ty, false),
        };
        let code = self.rows_read_typed(st, col, &target, payload_ty);
        let (ok_bb, null_bb, bad_bb, done) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        self.terminate(Terminator::Switch {
            value: code,
            cases: vec![(1, ok_bb), (2, null_bb)],
            default: bad_bb,
        });
        self.switch_to(ok_bb);
        if nullable {
            if let Ty::Agg(_) = self.cx.ty(ty) {
                self.assign(
                    crate::lower::operand::proj(fp, Proj::Field(0)),
                    Rvalue::Use(Self::ctrue()),
                );
            }
        }
        self.goto(done);
        self.switch_to(null_bb);
        if nullable {
            // Stays null (all-zero).
            self.goto(done);
        } else {
            self.goto(bad_bb);
        }
        self.switch_to(bad_bb);
        let expected = self.rows_expected(payload_ty);
        self.rows_fail(f, &expected, row, name, fail);
        self.switch_to(done);
    }

    /// The JSON decoder's name for what a `ty` reads.
    fn rows_expected(&mut self, ty: TyId) -> String {
        match self.cx.kind(ty) {
            TyKind::Int(it) => format!("{it:?}").to_lowercase(),
            TyKind::Float(_) => "number".into(),
            TyKind::Bool => "boolean".into(),
            TyKind::Str => "string".into(),
            _ => "array".into(),
        }
    }

    /// Reads column `col` as a `ty` into `target`: 1 (read), 2 (null), 0 (another kind, or an
    /// integer out of `ty`'s range).
    fn rows_read_typed(&mut self, st: Local, col: Operand, target: &Place, ty: TyId) -> Operand {
        let s = Operand::Copy(Place::local(st));
        let code = self.temp(Ty::U8);
        let vt = self.cx.ty(ty);
        match self.cx.kind(ty) {
            TyKind::Int(it) => {
                let t = self.temp(Ty::I64);
                let a = self.addr(Place::local(t));
                self.call_rt(Rt::RowsI64, vec![s, col, a], Some(Place::local(code)));
                let tv = Operand::Copy(Place::local(t));
                let c = self.cast_to(tv.clone(), Ty::I64, vt);
                if vt != Ty::I64 {
                    // Out of range: as another kind (the JSON decoder's message names the type).
                    let back = self.cast_to(c.clone(), vt, Ty::I64);
                    let mut fits =
                        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, back, tv.clone()));
                    if !it.is_signed() {
                        let nonneg = self
                            .rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ge, tv, cint(0, Ty::I64)));
                        fits =
                            self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, fits, nonneg));
                    }
                    let read = self.rvalue_temp(
                        Ty::Bool,
                        Rvalue::Binary(
                            BinOp::Eq,
                            Operand::Copy(Place::local(code)),
                            cint(1, Ty::U8),
                        ),
                    );
                    let unfit = self.rvalue_temp(
                        Ty::Bool,
                        Rvalue::Binary(
                            BinOp::Eq,
                            fits,
                            Operand::Const(Const::Bool(false), Ty::Bool),
                        ),
                    );
                    let out =
                        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, read, unfit));
                    let (bad, go) = (self.new_block(), self.new_block());
                    self.branch(out, bad, go);
                    self.switch_to(bad);
                    self.assign(Place::local(code), Rvalue::Use(cint(0, Ty::U8)));
                    self.goto(go);
                    self.switch_to(go);
                }
                self.assign(target.clone(), Rvalue::Use(c));
            }
            TyKind::Float(_) => {
                let t = self.temp(Ty::F64);
                let a = self.addr(Place::local(t));
                self.call_rt(Rt::RowsF64, vec![s, col, a], Some(Place::local(code)));
                let v = self.cast_to(Operand::Copy(Place::local(t)), Ty::F64, vt);
                self.assign(target.clone(), Rvalue::Use(v));
            }
            TyKind::Bool => {
                let t = self.temp(Ty::U8);
                self.assign(Place::local(t), Rvalue::Use(cint(0, Ty::U8)));
                let a = self.addr(Place::local(t));
                self.call_rt(Rt::RowsBool, vec![s, col, a], Some(Place::local(code)));
                let b = Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(t)), cint(0, Ty::U8));
                self.assign(target.clone(), b);
            }
            TyKind::Str => {
                let a = self.addr(target.clone());
                self.call_rt(Rt::RowsStr, vec![s, col, a], Some(Place::local(code)));
            }
            _ => {
                let arr = self.content(target, ty);
                let a = self.addr(arr);
                self.call_rt(Rt::RowsBytes, vec![s, col, a], Some(Place::local(code)));
            }
        }
        Operand::Copy(Place::local(code))
    }
}
