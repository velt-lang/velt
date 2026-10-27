//! std/db' `RowSource` written in Velt read by column (`__intrinsic_read_rows<T, S>`): the
//! source's own methods called for each value, a column map from its column names.

use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::super::STR;
use super::*;
use crate::lower::rt::Rt;
use crate::lower::{cint, ice, FnLower};
use crate::vir::{BlockId, Local, Operand, Place, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// [`Self::rows_fetch`] from a `RowSource`: its `kind(col)`, then the getter of that kind
    /// (an integer also from an exact float, a float also from an integer).
    pub(super) fn rows_fetch_velt(
        &mut self,
        src: &RowSrc,
        col: Operand,
        fetch: Fetch,
        target: &Place,
        code: Local,
    ) {
        self.assign(Place::local(code), Rvalue::Use(cint(0, Ty::U8)));
        // Columns count from 0 as `i64`s for the source.
        let col = self.cast_to(col, Ty::U64, Ty::I64);
        let (kp, kty) = self.rows_call(src, KIND, Some(col.clone()));
        let kt = self.cx.ty(kty);
        let k = self.cast_to(Operand::Copy(kp), kt, Ty::I64);
        let done = self.new_block();
        let null_bb = self.new_block();
        let mut cases = vec![(KIND_NULL, null_bb)];
        let mut take = |lw: &mut Self, kind: i128| {
            let b = lw.new_block();
            cases.push((kind, b));
            b
        };
        let ok = |lw: &mut Self| {
            lw.assign(Place::local(code), Rvalue::Use(cint(1, Ty::U8)));
            lw.goto(done);
        };
        let mut arms: Vec<(BlockId, usize)> = Vec::new();
        match fetch {
            Fetch::I64(_) => {
                arms.push((take(self, KIND_INT), GET_INT));
                arms.push((take(self, KIND_FLOAT), GET_FLOAT));
            }
            Fetch::F64(_) => {
                arms.push((take(self, KIND_INT), GET_INT));
                arms.push((take(self, KIND_FLOAT), GET_FLOAT));
            }
            Fetch::Bool(_) => arms.push((take(self, KIND_BOOL), GET_BOOL)),
            Fetch::Str => arms.push((take(self, KIND_TEXT), GET_TEXT)),
            Fetch::Bytes => arms.push((take(self, KIND_BLOB), GET_BLOB)),
        }
        self.terminate(Terminator::Switch {
            value: k,
            cases,
            default: done,
        });
        self.switch_to(null_bb);
        self.assign(Place::local(code), Rvalue::Use(cint(2, Ty::U8)));
        self.goto(done);
        for (b, getter) in arms {
            self.switch_to(b);
            let (vp, vty) = self.rows_call(src, getter, Some(col.clone()));
            let vt = self.cx.ty(vty);
            let v = Operand::Copy(vp);
            match (&fetch, getter) {
                (Fetch::I64(t), GET_INT) => {
                    let v = self.cast_to(v, vt, Ty::I64);
                    self.assign(Place::local(*t), Rvalue::Use(v));
                    ok(self);
                }
                (Fetch::I64(t), _) => {
                    // A float that is an integer exactly.
                    let f = self.cast_to(v, vt, Ty::F64);
                    let a = self.addr(Place::local(*t));
                    self.call_rt(Rt::RowsExactI64, vec![f, a], Some(Place::local(code)));
                    self.goto(done);
                }
                (Fetch::F64(t), _) => {
                    let v = self.cast_to(v, vt, Ty::F64);
                    self.assign(Place::local(*t), Rvalue::Use(v));
                    ok(self);
                }
                (Fetch::Bool(t), _) => {
                    let v = self.cast_to(v, vt, Ty::U8);
                    self.assign(Place::local(*t), Rvalue::Use(v));
                    ok(self);
                }
                // A string or `u8[]` the getter returned (owned): moved into the field.
                (Fetch::Str | Fetch::Bytes, _) => {
                    self.assign(target.clone(), Rvalue::Use(v));
                    ok(self);
                }
            }
        }
        self.switch_to(done);
    }

    /// Calls method `which` of the `RowSource` with the column `col` (when it takes one): the
    /// place of its (owned) result and the result's type.
    pub(super) fn rows_call(
        &mut self,
        src: &RowSrc,
        which: usize,
        col: Option<Operand>,
    ) -> (Place, TyId) {
        let RowSrc::Velt {
            obj, cls, methods, ..
        } = src
        else {
            ice("a RowSource call on a native cursor")
        };
        let m = methods[which];
        let ret = self.hook_result(m, *cls);
        let targs = self.cx.method_targs(m, *cls);
        let modes: Vec<hir::PassMode> = self.cx.fn_def(m).params.iter().map(|p| p.mode).collect();
        let this = match modes.first() {
            Some(hir::PassMode::Owned) => self.share_value(obj.clone(), *cls),
            _ => obj.clone(),
        };
        let mut args = vec![this];
        args.extend(col);
        let f = self.cx.func_for_direct(m, targs, self.mode);
        let abi = self.cx.ret_abi(ret, None);
        if let Some(out) = abi.out {
            let tmp = self.temp(out);
            let a = self.addr(Place::local(tmp));
            args.push(a);
            self.call(crate::vir::Callee::Func(f), args, None, false);
            return (Place::local(tmp), ret);
        }
        let d = self.temp(abi.ret);
        let dest = (abi.ret != Ty::Unit).then(|| Place::local(d));
        self.call(crate::vir::Callee::Func(f), args, dest, false);
        (Place::local(d), ret)
    }

    /// The `RowSource` methods of `cls`, when rows can be read from it by column: a class that
    /// implements std/db' `RowSource` itself (not generically), whose methods do not throw.
    fn rows_source_methods(&mut self, cls: TyId) -> Option<Vec<DefId>> {
        if !self.cx.is_class(cls) {
            return None;
        }
        let iface = self
            .cx
            .hir
            .defs
            .iter()
            .position(|d| matches!(d, hir::Def::Interface(i) if i.name == ROW_SOURCE))?;
        let imp = self
            .cx
            .hir
            .impls
            .iter()
            .find(|i| i.iface.0 as usize == iface && i.generics == 0 && i.ty == cls)?;
        let methods = imp.methods.clone();
        let throws = methods.iter().any(|m| {
            self.cx
                .fn_def(*m)
                .throws
                .is_some_and(|t| !matches!(self.cx.kind(t), TyKind::Never))
        });
        (methods.len() >= 10 && !throws).then_some(methods)
    }

    /// `__intrinsic_read_rows<T, S>(src)` (every remaining row, `many`) or
    /// `__intrinsic_read_row<T, S>(src)` (the current row), of type `ty` (`T[] | null` or
    /// `T | null`): read by column from the `RowSource` `src`, or null (nothing read) when rows
    /// of `T` cannot be read so from an `S`; a row that does not fit throws `JsonError`.
    pub(in crate::lower) fn read_rows(&mut self, a: &hir::Expr, many: bool, ty: TyId) -> Operand {
        let oty = self.sub(ty);
        let TyKind::Option(t, _) = self.cx.kind(oty) else {
            ice("read_rows of a non-option type")
        };
        let row_ty = match (many, self.cx.kind(t)) {
            (true, TyKind::Array(e)) => e,
            (true, _) => ice("read_rows of a non-array type"),
            (false, _) => t,
        };
        let cls = self.sub(a.ty);
        let v = self.expr(a);
        if self.dead() {
            return crate::lower::unit();
        }
        let ovt = self.cx.ty(oty);
        let Some(methods) = self
            .rows_source_methods(cls)
            .filter(|_| self.rows_row_ok(row_ty))
        else {
            // Read as JSON by std/db.
            return self.zero_value(ovt);
        };
        let vt = self.cx.ty(t);
        let out = self.temp(vt);
        self.json_init(out, t);
        let obj_place = self.place_of(v, cls);
        let obj = Operand::Copy(obj_place);
        // The column map, from the source's column names.
        let table = self.rows_field_table(row_ty);
        let st = self.temp(Ty::Ptr);
        self.call_rt(Rt::RowsMap, vec![table], Some(Place::local(st)));
        let ncols = self.temp(Ty::U64);
        let src = RowSrc::Velt {
            st,
            obj,
            cls,
            methods,
            ncols,
        };
        let (np, nty) = self.rows_call(&src, COLUMN_COUNT, None);
        let nt = self.cx.ty(nty);
        let n = self.cast_to(Operand::Copy(np), nt, Ty::U64);
        self.assign(Place::local(ncols), Rvalue::Use(n));
        let c = self.temp(Ty::U64);
        self.assign(Place::local(c), Rvalue::Use(cint(0, Ty::U64)));
        let len = Operand::Copy(Place::local(ncols));
        self.count_loop(c, len, |lw, cv| {
            let ci = lw.cast_to(cv, Ty::U64, Ty::I64);
            let (name, sty) = lw.rows_call(&src, COLUMN_NAME, Some(ci));
            let na = lw.addr(name.clone());
            let s = Operand::Copy(Place::local(st));
            lw.call_rt(Rt::RowsMapColumn, vec![s, na], None);
            lw.drop_glue(name, sty);
        });
        let f = self.rows_failure();
        let (fail, done, ok_bb) = (self.new_block(), self.new_block(), self.new_block());
        let place = Place::local(out);
        if many {
            self.rows_read_all(&src, &place, row_ty, f, fail, done);
        } else {
            self.rows_read_row(&src, &place, row_ty, cint(-1, Ty::I64), f, fail);
            self.goto(done);
        }
        let s = Operand::Copy(Place::local(st));
        self.switch_to(done);
        self.call_rt(Rt::RowsEnd, vec![s.clone()], None);
        self.goto(ok_bb);
        self.switch_to(fail);
        self.call_rt(Rt::RowsEnd, vec![s], None);
        self.drop_glue(place.clone(), t);
        self.json_zero(&place, t);
        let err = self.temp(STR);
        let ea = self.addr(Place::local(err));
        self.rows_message(f, ea);
        let e = self.json_error_value(Operand::Copy(Place::local(err)));
        self.record_throw_loc();
        let et = self.cx.json_error_ty();
        self.route_error(e, et);
        self.switch_to(ok_bb);
        // The result, owned (as `wrap_some` makes it): the option of the rows, a class row its
        // own option.
        let v = Operand::Copy(Place::local(out));
        match ovt {
            Ty::Ptr => self.own_value(v, oty),
            _ => self.build_agg(oty, vec![Self::ctrue(), v]),
        }
    }
}
