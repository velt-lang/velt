//! The exported entry `velt_main() -> i32`: calls the user `main` and returns its `i32` result,
//! or 0 for a `void` main. An error escaping a throwing `main` prints
//! `Uncaught <Type>` (plus `: <message>` when the type has a `message: string` field) to stderr,
//! is dropped, and makes the process exit with 1. An `async main` runs its state machine with
//! `velt_rt_block_on` (state on `velt_main`'s stack) and reads the result at `state + 0`.
//!
//! Before anything else, `velt_main` initializes the native libraries of packages
//! (native_abi.md "Start-up"): for each, `rc = velt_native_init_<pkg>(velt_rt_native_api())`
//! then `velt_rt_native_check(rc, &"<pkg>")`, which stops the program when `rc != 0`.

use velt_sema::hir::{self, TyId, TyKind};

use super::glue::SLOT_NAME;
use super::operand::proj;
use super::rt::Rt;
use super::{cint, FnLower, ScopeKind};
use crate::vir::{self, BinOp, Function, Linkage, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl<'c, 'h> FnLower<'c, 'h> {
    pub(super) fn build_main(cx: &'c mut super::Cx<'h>) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        lw.init_natives();
        let Some(def) = lw.cx.hir.entry else {
            lw.terminate(Terminator::Return(cint(0, Ty::I32)));
            return lw.finish_export();
        };
        let f = lw.cx.fn_def(def);
        if f.is_async {
            lw.push_scope(ScopeKind::Block);
            let code = lw.run_async_main(def);
            lw.terminate(Terminator::Return(code));
            lw.scopes.clear();
            return lw.finish_export();
        }
        let (ret, throws) = (f.ret, f.throws);
        let fid = lw.cx.func_for(def, vec![]);
        let abi = lw.cx.ret_abi(ret, throws);
        lw.push_scope(ScopeKind::Block);
        let callee = vir::Callee::Func(fid);
        let never = lw.cx.is_never(ret) && throws.is_none();
        let code = match (abi.out, throws) {
            (Some(out), Some(err)) => {
                let tmp = lw.temp(out);
                let a = lw.addr(Place::local(tmp));
                lw.call(callee, vec![a], None, false);
                lw.uncaught_check(tmp, ret, err)
            }
            (_, _) if abi.ret == Ty::I32 => {
                let d = lw.temp(Ty::I32);
                lw.call(callee, vec![], Some(Place::local(d)), never);
                Operand::Copy(Place::local(d))
            }
            _ => {
                lw.call(callee, vec![], None, never);
                cint(0, Ty::I32)
            }
        };
        lw.terminate(Terminator::Return(code));
        lw.scopes.clear();
        lw.finish_export()
    }

    /// `velt_rt_block_on(main$poll, &state)`; yields the exit code.
    fn run_async_main(&mut self, def: hir::DefId) -> Operand {
        let info = self
            .cx
            .async_info(def, &[])
            .unwrap_or_else(|| super::ice("async main has no state layout"));
        let s = self.temp(Ty::Agg(info.state));
        self.init_state(&info, &Place::local(s), vec![]);
        let a = self.addr(Place::local(s));
        self.call_rt(Rt::BlockOn, vec![super::cfunc(info.poll), a.clone()], None);
        let f = self.cx.fn_def(def);
        let ret = self.cx.async_result(f);
        let res = proj(&self.operand_place(a, Ty::Ptr), Proj::Deref(info.result));
        match f.throws {
            Some(err) => {
                let t = self.copy_to_temp(Operand::Copy(res), info.result);
                self.uncaught_check(t, ret, err)
            }
            None if info.result == Ty::I32 => Operand::Copy(res),
            None => cint(0, Ty::I32),
        }
    }

    /// Call every native library's init with the runtime table, checking each result.
    fn init_natives(&mut self) {
        let inits = self.cx.native_inits.clone();
        if inits.is_empty() {
            return;
        }
        let api_fn = self
            .cx
            .extern_sym("velt_rt_native_api", vec![], Ty::Ptr, false);
        let check = self.cx.extern_sym(
            "velt_rt_native_check",
            vec![Ty::I32, Ty::Ptr],
            Ty::Unit,
            false,
        );
        for init in inits {
            let api = self.temp(Ty::Ptr);
            self.call(
                vir::Callee::Extern(api_fn),
                vec![],
                Some(Place::local(api)),
                false,
            );
            let f = self
                .cx
                .extern_sym(&init.symbol, vec![Ty::Ptr], Ty::I32, false);
            let rc = self.temp(Ty::I32);
            let args = vec![Operand::Copy(Place::local(api))];
            self.call(vir::Callee::Extern(f), args, Some(Place::local(rc)), false);
            let name = self.cx.static_str_object(&init.package);
            let name = Operand::Const(vir::Const::Static(name), Ty::Ptr);
            let args = vec![Operand::Copy(Place::local(rc)), name];
            self.call(vir::Callee::Extern(check), args, None, false);
        }
    }

    fn finish_export(self) -> Function {
        let mut f = self.finish("velt_main".into(), vec![], Ty::I32);
        f.linkage = Linkage::Export;
        f
    }

    /// Handle the `Result` of a throwing `main`; yields the exit code for the Ok case.
    fn uncaught_check(&mut self, res: vir::Local, ret: TyId, err: TyId) -> Operand {
        let rty = self.cx.intern(TyKind::Result(ret, err));
        let rp = Place::local(res);
        let tag = Operand::Copy(proj(&rp, Proj::Field(0)));
        let is_err = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, tag, cint(0, Ty::U8)));
        let (err_bb, ok_bb) = (self.new_block(), self.new_block());
        self.branch(is_err, err_bb, ok_bb);
        self.switch_to(err_bb);
        let ev = self.cx.view(rty, 1);
        let ep = proj(&proj(&rp, Proj::Cast(ev)), Proj::Field(1));
        self.report_uncaught(&ep, err);
        self.drop_glue(ep, err);
        self.terminate(Terminator::Return(cint(1, Ty::I32)));
        self.switch_to(ok_bb);
        match self.cx.ty(ret) {
            Ty::I32 => {
                let ov = self.cx.view(rty, 0);
                let p = proj(&proj(&rp, Proj::Cast(ov)), Proj::Field(1));
                Operand::Copy(p)
            }
            _ => cint(0, Ty::I32),
        }
    }

    /// `Uncaught <Type>[: message]` on stderr for the error value at `ep` (for a union, of the
    /// member it holds).
    pub(super) fn report_uncaught(&mut self, ep: &Place, err: TyId) {
        if self.cx.is_union(err) {
            let done = self.new_block();
            self.for_each_member(ep, err, |lw, payload, m| {
                let vt = lw.cx.ty(m);
                let p = match vt {
                    Ty::Unit => Place::local(lw.temp(Ty::U8)),
                    _ => Place::local(lw.copy_to_temp(payload, vt)),
                };
                lw.report_uncaught(&p, m);
                lw.goto(done);
            });
            self.switch_to(done);
            return;
        }
        let stream = cint(2, Ty::U32);
        self.write_text(&stream, "Uncaught ");
        self.write_error_name(&stream, ep, err);
        if let Some(i) = self.message_field(err) {
            self.write_text(&stream, ": ");
            let mp = self.field_place(ep, err, i);
            let a = self.addr(mp);
            self.call_rt(Rt::WriteStr, vec![stream.clone(), a], None);
        }
        self.write_throw_loc(&stream);
        self.call_rt(
            Rt::WriteByte,
            vec![stream, cint(b'\n' as i128, Ty::U8)],
            None,
        );
    }

    /// The name of the error's type; for a class with subclasses, of its dynamic class (read from
    /// its vtable), so an `IoError` thrown as an `Error` reports `IoError`.
    fn write_error_name(&mut self, stream: &Operand, ep: &Place, err: TyId) {
        let dynamic = match self.cx.kind(err) {
            TyKind::Adt(d, _) => self.cx.is_class(err) && self.cx.has_header(d),
            _ => false,
        };
        if !dynamic {
            let name = self.cx.type_name(err);
            self.write_text(stream, &name);
            return;
        }
        let vt = self.obj_vtable(Operand::Copy(ep.clone()), err);
        let name = self.dispatch(vt, SLOT_NAME);
        self.call_rt(Rt::WriteStr, vec![stream.clone(), name], None);
    }

    /// ` at <path>:<line>:<col>` of the last `throw` on this thread, if recorded.
    fn write_throw_loc(&mut self, stream: &Operand) {
        if self.cx.locs.is_none() {
            return;
        }
        let p = self.temp(Ty::Ptr);
        self.call_rt(Rt::ThrowLoc, vec![], Some(Place::local(p)));
        let known = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(p)), cint(0, Ty::Ptr)),
        );
        let (write_bb, done) = (self.new_block(), self.new_block());
        self.branch(known, write_bb, done);
        self.switch_to(write_bb);
        let args = vec![stream.clone(), Operand::Copy(Place::local(p))];
        self.call_rt(Rt::WriteStr, args, None);
        self.goto(done);
        self.switch_to(done);
    }

    /// Index of a `message: string` field of a struct/class error type.
    fn message_field(&mut self, err: TyId) -> Option<u32> {
        let TyKind::Adt(d, _) = self.cx.kind(err) else {
            return None;
        };
        let hir::Def::Adt(a) = self.cx.hir.def(d) else {
            return None;
        };
        let i = a.fields.iter().position(|f| f.name == "message")?;
        let fty = self.cx.adt_field_tys(err)[i];
        matches!(self.cx.kind(fty), TyKind::Str).then_some(i as u32)
    }
}
