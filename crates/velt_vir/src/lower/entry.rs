//! The exported entry `velt_main() -> i32`: calls the user `main` and returns its `i32` result,
//! or 0 for a `void` main. An error escaping a throwing `main` prints `Uncaught <Name>: <message>`
//! for an `Error` (Node's first line), or `Uncaught <value>` for any other thrown value as
//! `console.log` prints it (Node prints `a string` for `throw "a string"`), to stderr, is dropped,
//! and makes the process exit with 1. An `async main` runs its state machine with
//! `velt_rt_block_on` (state on `velt_main`'s stack) and reads the result at `state + 0`. So does
//! a synchronous `main` of a program that starts promises or tasks (timers, microtasks, `spawn`):
//! its root task (`SyncMainRoot`) calls `main` and stores the result, so what `main` starts runs
//! on that task, as in Node, where `main end` prints before the callbacks `main` queued. A
//! program that never does calls `main` directly.
//!
//! Before it returns, `velt_main` hands the runtime the function releasing what static atomics
//! hold at exit, when there are any (static_atomic.rs).
//!
//! Before anything else, `velt_main` initializes the native libraries of packages
//! (native_abi.md "Start-up"): for each, `rc = velt_native_init_<pkg>(velt_rt_native_api())`
//! then `velt_rt_native_check(rc, &"<pkg>")`, which stops the program when `rc != 0`.

use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::glue::SLOT_NAME;
use super::operand::proj;
use super::rt::Rt;
use super::{cint, FnLower, ScopeKind, Work};
use crate::vir::{self, BinOp, Function, Linkage, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl<'c, 'h> FnLower<'c, 'h> {
    pub(super) fn build_main(cx: &'c mut super::Cx<'h>) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        if lw.cx.uses_placement {
            // Before anything starts the runtime (#211).
            lw.call_rt(Rt::SetScheduler, vec![cint(1, Ty::U32)], None);
        }
        lw.init_natives();
        let Some(def) = lw.cx.hir.entry else {
            lw.terminate(Terminator::Return(cint(0, Ty::I32)));
            return lw.finish_export();
        };
        // Every module's initializers run before `main`, as Node evaluates the modules first
        // (one module state per process; an async `main` hands it to the tasks).
        lw.init_module_state();
        let f = lw.cx.fn_def(def);
        if f.is_async {
            lw.push_scope(ScopeKind::Block);
            let code = lw.run_async_main(def);
            lw.register_statics_drop();
            lw.terminate(Terminator::Return(code));
            lw.scopes.clear();
            return lw.finish_export();
        }
        let (ret, throws) = (f.ret, f.throws);
        let abi = lw.cx.ret_abi(ret, throws);
        lw.push_scope(ScopeKind::Block);
        // Whether `main` (or a module initializer) reaches promises or tasks: everything else is
        // built by now except what the module initializers call.
        while let Some((fid, work)) = lw.cx.queue.pop_front() {
            lw.cx.build_now(fid, &work);
        }
        let code = if lw.cx.reaches_tasks() {
            let out = match (abi.out, throws) {
                (Some(out), Some(_)) => out,
                _ if abi.ret == Ty::I32 => Ty::I32,
                _ => Ty::U8,
            };
            let s = lw.temp(out);
            let a = lw.addr(Place::local(s));
            let root = lw.cx.func(Work::SyncMainRoot);
            lw.call_rt(Rt::BlockOn, vec![super::cfunc(root), a], None);
            match throws {
                Some(err) if abi.out.is_some() => lw.uncaught_check(s, ret, err),
                _ if abi.ret == Ty::I32 => Operand::Copy(Place::local(s)),
                _ => cint(0, Ty::I32),
            }
        } else {
            lw.call_main(def, None)
        };
        lw.register_statics_drop();
        lw.terminate(Terminator::Return(code));
        lw.scopes.clear();
        lw.finish_export()
    }

    /// Calls the synchronous `main` `def`: with `state` (the root of `SyncMainRoot`), stores its
    /// result there; else yields the exit code.
    fn call_main(&mut self, def: DefId, state: Option<vir::Local>) -> Operand {
        let f = self.cx.fn_def(def);
        let (ret, throws) = (f.ret, f.throws);
        // A synchronous `main` runs outside read mode (`Work::Checked`).
        let fid = self.cx.func_for_direct(def, vec![], super::LkMode::Plain);
        let abi = self.cx.ret_abi(ret, throws);
        let callee = vir::Callee::Func(fid);
        let never = self.cx.is_never(ret) && throws.is_none();
        match (abi.out, throws) {
            (Some(out), Some(err)) => {
                if let Some(sp) = state {
                    self.call(callee, vec![Operand::Copy(Place::local(sp))], None, false);
                    return cint(0, Ty::I32);
                }
                let tmp = self.temp(out);
                let a = self.addr(Place::local(tmp));
                self.call(callee, vec![a], None, false);
                self.uncaught_check(tmp, ret, err)
            }
            (_, _) if abi.ret == Ty::I32 => {
                let d = match state {
                    Some(sp) => proj(&Place::local(sp), Proj::Deref(Ty::I32)),
                    None => Place::local(self.temp(Ty::I32)),
                };
                self.call(callee, vec![], Some(d.clone()), never);
                Operand::Copy(d)
            }
            _ => {
                self.call(callee, vec![], None, never);
                cint(0, Ty::I32)
            }
        }
    }

    /// `(state, cx) -> READY`: the root task of a synchronous `main` that reaches promises or
    /// tasks (`build_main`).
    pub(super) fn build_sync_main_root(cx: &'c mut super::Cx<'h>) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        let state = lw.new_local(Ty::Ptr, Some("state".into()));
        lw.new_local(Ty::Ptr, Some("cx".into()));
        let def = lw
            .cx
            .hir
            .entry
            .unwrap_or_else(|| super::ice("a main root without a main"));
        lw.push_scope(ScopeKind::Block);
        lw.call_main(def, Some(state));
        lw.terminate(Terminator::Return(cint(1, Ty::U32)));
        lw.scopes.clear();
        lw.finish("_Gsync_main_root".into(), vec![Ty::Ptr, Ty::Ptr], Ty::U32)
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
    pub(super) fn uncaught_check(&mut self, res: vir::Local, ret: TyId, err: TyId) -> Operand {
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

    /// `Uncaught <Name>: <message>` (an `Error`) or `Uncaught <value>` (anything else) on stderr
    /// for the error value at `ep` (for a union, of the member it holds).
    pub(super) fn report_uncaught(&mut self, ep: &Place, err: TyId) {
        // Only the first uncaught error is reported: another thread failing at the same time
        // waits for the process to end.
        self.call_rt(Rt::UncaughtReport, vec![], None);
        // Built whole and written in one write, so no other thread's output lands inside it.
        let (_, buf) = self.new_strbuf(0);
        self.push_text(&buf, "Uncaught ");
        self.push_error_text(&buf, ep, err);
        self.push_throw_loc(&buf);
        self.push_text(&buf, "\n");
        const STR: Ty = Ty::Agg(crate::vir::STR_AGG);
        let text = self.temp(STR);
        let tp = self.addr(Place::local(text));
        self.call_rt(Rt::StrbufFinish, vec![buf, tp.clone()], None);
        self.call_rt(Rt::WriteStr, vec![cint(2, Ty::U32), tp.clone()], None);
        self.call_rt(Rt::StrDrop, vec![tp], None);
    }

    /// What an uncaught report writes after `Uncaught ` for the error value at `ep` (for a
    /// union, of the member it holds), appended to the builder at `buf`: Node's header for an
    /// `Error`, else the value (the location follows: `push_throw_loc`). Also
    /// `Intrinsic::ErrorText`, so a test runner reports a caught error as it would be reported
    /// uncaught.
    pub(super) fn push_error_text(&mut self, buf: &Operand, ep: &Place, err: TyId) {
        if self.cx.is_union(err) {
            if let Some(root) = self.common_error_root(err) {
                return self.push_union_error_header(buf, ep, err, root);
            }
            let done = self.new_block();
            self.for_each_member(ep, err, |lw, payload, m| {
                let vt = lw.cx.ty(m);
                let p = match vt {
                    Ty::Unit => Place::local(lw.temp(Ty::U8)),
                    _ => Place::local(lw.copy_to_temp(payload, vt)),
                };
                lw.push_error_text(buf, &p, m);
                lw.goto(done);
            });
            self.switch_to(done);
            return;
        }
        if let Some(root) = self.error_root(err) {
            self.push_error_header(buf, ep, err, root);
        } else if self.cx.is_dynamic(err) {
            self.push_dynamic_thrown(buf, ep, err);
        } else {
            self.push_thrown_value(buf, ep, err);
        }
    }

    /// The `Error` class every member of the union `err` extends, when they all do (the errors
    /// `velt/fs` throws: `IoError | RangeError | TypeError`).
    fn common_error_root(&mut self, err: TyId) -> Option<DefId> {
        let mut root = None;
        for k in 0..self.union_len(err) {
            let m = self.cx.variant_tys(err, k)[0];
            if self.cx.is_never(m) {
                continue;
            }
            let r = self.error_root(m)?;
            if root.is_some_and(|x| x != r) {
                return None;
            }
            root = Some(r);
        }
        root
    }

    /// [`push_error_text`](Self::push_error_text) for a union of `Error` classes extending
    /// `root`: each member only names its class, and one call writes the header, so a `main`
    /// calling functions that throw three kinds of error has one header, not three.
    fn push_union_error_header(&mut self, buf: &Operand, ep: &Place, err: TyId, root: DefId) {
        const STR: Ty = Ty::Agg(crate::vir::STR_AGG);
        let obj = Place::local(self.temp(Ty::Ptr));
        let name = Place::local(self.temp(STR));
        let tail = self.new_block();
        self.for_each_member(ep, err, |lw, payload, m| {
            lw.assign(obj.clone(), Rvalue::Use(payload));
            let cls = lw.error_class_name(&obj, m);
            lw.assign(name.clone(), Rvalue::Use(Operand::Copy(cls)));
            lw.goto(tail);
        });
        self.terminate(Terminator::Unreachable);
        self.switch_to(tail);
        self.push_class_error_header(buf, &obj, root, &name);
    }

    /// A thrown `any` (or `unknown`) value at `ep`: Node's header for an `Error` it holds
    /// (`RangeError: outer`), else the value as [`push_thrown_value`](Self::push_thrown_value)
    /// writes it, appended to the builder at `buf`.
    fn push_dynamic_thrown(&mut self, buf: &Operand, ep: &Place, err: TyId) {
        let Some(root) = self.cx.prelude_error() else {
            return self.push_thrown_value(buf, ep, err);
        };
        let is_error = self.instance_test(ep, err, root);
        let (error_bb, value_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is_error, error_bb, value_bb);
        self.switch_to(error_bb);
        let obj = self.downcast_place(ep.clone(), err);
        let error = self.cx.intern(TyKind::Adt(root, vec![]));
        self.push_error_header(buf, &obj, error, root);
        self.goto(done);
        self.switch_to(value_bb);
        self.push_thrown_value(buf, ep, err);
        self.goto(done);
        self.switch_to(done);
    }

    /// A thrown value that is not an `Error` as Node reports it: as `console.log` prints it
    /// (`throw "a string"` reports `a string`, an object `{ code: 1 }`), a number as `String(x)`.
    fn push_thrown_value(&mut self, buf: &Operand, ep: &Place, err: TyId) {
        // A thrown number is written as `String(x)` writes it (Node prints `0` for `throw -0`).
        let number = match self.cx.kind(err) {
            TyKind::Float(_) => true,
            TyKind::Option(e, _) => matches!(self.cx.kind(e), TyKind::Float(_)),
            _ => false,
        };
        if number {
            self.format_top(buf, ep, err);
        } else {
            self.log_top(buf, ep, err);
        }
    }

    /// The first line Node prints for an uncaught error of class `err` (extending the prelude
    /// `Error` class `root`): `Error.__uncaughtHeader(<dynamic class name>)`, which writes
    /// `<name>: <message>` from the (possibly overridden) `name`, and the class in front when
    /// they differ (`E [TypeError]: m`).
    fn push_error_header(&mut self, buf: &Operand, ep: &Place, err: TyId, root: DefId) {
        let cls = self.error_class_name(ep, err);
        self.push_class_error_header(buf, ep, root, &cls);
    }

    /// The name of the class of the error of class type `err` at `ep` that the header shows (a
    /// string the place holds, not owned).
    fn error_class_name(&mut self, ep: &Place, err: TyId) -> Place {
        // A standard library class stands for the error Node throws (`UrlError` for a
        // `TypeError`): named as that class, so the header is its `name` alone, as for
        // `JsonError`.
        let std_class = match self.cx.kind(err) {
            TyKind::Adt(d, _) => self.cx.std_error_class(d),
            _ => None,
        };
        let dynamic = match self.cx.kind(err) {
            TyKind::Adt(d, _) => std_class.is_none() && self.cx.has_header(d),
            _ => false,
        };
        if self.is_json_error(err) {
            // `JSON.parse`'s error stands for the `SyntaxError` Node throws: `SyntaxError: <m>`.
            let s = self.str_lit("SyntaxError");
            Place::local(self.copy_to_temp(s, Ty::Agg(crate::vir::STR_AGG)))
        } else if dynamic {
            let vt = self.obj_vtable(Operand::Copy(ep.clone()), err);
            let p = self.dispatch(vt, SLOT_NAME);
            let pp = self.operand_place(p, Ty::Ptr);
            Place {
                local: pp.local,
                proj: vec![Proj::Deref(Ty::Agg(crate::vir::STR_AGG))],
            }
        } else {
            let name = std_class.unwrap_or_else(|| self.cx.type_name(err));
            let s = self.str_lit(&name);
            Place::local(self.copy_to_temp(s, Ty::Agg(crate::vir::STR_AGG)))
        }
    }

    /// `Error.__uncaughtHeader(cls)` of the error at `ep` (a class extending `root`), appended
    /// to the builder at `buf`.
    fn push_class_error_header(&mut self, buf: &Operand, ep: &Place, root: DefId, cls: &Place) {
        let m = self.class_method(root, "__uncaughtHeader");
        let root_ty = self.cx.intern(TyKind::Adt(root, vec![]));
        let (text, ty) = self.call_hook(m, Operand::Copy(ep.clone()), root_ty, Some(cls));
        let a = self.addr(text.clone());
        self.push_str(buf, a);
        self.drop_glue(text, ty);
    }

    /// Is `err` the prelude's `JsonError` (what `JSON.parse` throws), not a class of that name?
    fn is_json_error(&mut self, err: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(err) else {
            return false;
        };
        self.cx.json_error_def() == Some(d)
    }

    /// ` at <path>:<line>:<col>` of the last `throw` on this thread, if recorded.
    pub(super) fn push_throw_loc(&mut self, buf: &Operand) {
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
        self.push_str(buf, Operand::Copy(Place::local(p)));
        self.goto(done);
        self.switch_to(done);
    }

    /// The prelude `Error` class when class `err` is it or extends it.
    fn error_root(&mut self, err: TyId) -> Option<DefId> {
        let TyKind::Adt(mut d, _) = self.cx.kind(err) else {
            return None;
        };
        if !self.cx.is_class(err) {
            return None;
        }
        // The root class: its fields are a prefix of every subclass's.
        while let Some(b) = self.cx.adt_def(d).base {
            let TyKind::Adt(bd, _) = self.cx.kind(b) else {
                return None;
            };
            d = bd;
        }
        let root = self.cx.adt_def(d);
        let is_error = (root.name == "Error" || root.name.ends_with("::Error"))
            && root.fields.len() == 2
            && root.fields[0].name == "message"
            && root.fields[1].name == "#name";
        is_error.then_some(d)
    }
}
