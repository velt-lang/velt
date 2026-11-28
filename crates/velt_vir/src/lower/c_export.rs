//! The functions C calls (`@export("c_name")`, #2203 step 5, docs/internals/design/c-ffi.md
//! "Exported functions"): for each `hir::CExport`, a function with the unmangled symbol `c_name`
//! (`Linkage::Export`) and the C signature sema checked, built whether or not Velt code reaches
//! it. Velt functions already use the target's C calling convention for scalars, so the function
//! only checks what crosses as a `declare c` call does in the other direction (c_call.rs): a
//! `Ptr<T>` parameter C passes `NULL` for stops the program (C has no non-null pointer, and the
//! word would read as `null`, #3094); pointers are the same word on both sides. It calls the
//! exported function as any call does, with its own ABI (a result through an out-pointer).
//!
//! A panic that reaches it ends the process: nothing unwinds into C (owner decision (b) on
//! #2649, until #2008). A panic ends the process anyway unless a `catchPanic` poll is up the
//! thread's stack, which only a C function called from Velt can put there; for that case the
//! function is marked `Function::stops_unwind`, and the backends abort when an unwind reaches it
//! (LLVM: a landing pad calling `abort`; Cranelift: no unwind information, so the unwinder can't
//! pass it).
//!
//! WebAssembly has no C callers to link with: an export there is an error (a module export
//! of the function would come with the host's own ABI, not C's).

use velt_sema::hir::{self, CTy, ExprKind as E, LocalId, UseMode};

use super::c_call;
use super::{unit, Cx, FnLower, LInfo, LState, Work};
use crate::vir::{Const, Function, Linkage, Operand, Rvalue, Terminator, Ty};

/// The prefix of the executor in Velt's exports (std/lang/hooks/sched*.velt), which the runtime
/// calls into when a program runs on it.
const SCHED_EXPORTS: &str = "std_sched_";

/// The exports of the modules std calls by role (`hookCall`, #2882), which a program loads only
/// when live code calls them, as std loaded on demand does (the blocking pool's thread function).
/// Not seeded with the executor's: what starts their threads takes their address
/// (`exportedAddress`), which builds them. So a program loaded with std up front, where these
/// modules are loaded whatever it calls, builds the same functions as on demand.
const BY_ROLE_EXPORTS: &[&str] = &["std_sched_blocking"];

/// A JS host's host-promise leaf (sched_host.velt `hostPoll`, `hostDrop`), whose functions call the
/// glue: built by its `exportedAddress` only, never seeded (WASI loads the file too, unused).
const HOST_LEAF_EXPORTS: &[&str] = &["std_sched_host_poll", "std_sched_host_drop"];

/// The turns a JS host calls (sched_host.velt, the glue's `velt_turn()` and `velt_settle(id)`):
/// module exports of a JS host's program that runs on the executor in Velt, and nowhere else.
const HOST_TURN_EXPORTS: &[&str] = &["velt_turn", "velt_settle"];

impl Cx<'_> {
    /// Queues the C function of every `@export` of the program (on WebAssembly, reports each).
    pub(super) fn seed_c_exports(&mut self) {
        if self.hir.c_exports.is_empty() {
            return;
        }
        if self.target.trim().starts_with("wasm") {
            if super::host::js_host(&self.target) {
                for i in 0..self.hir.c_exports.len() {
                    if HOST_TURN_EXPORTS.contains(&self.hir.c_exports[i].name.as_str()) {
                        self.func(Work::CExport(i as u32));
                    }
                }
            }
            // The executor in Velt's exports (std/lang/hooks/sched*.velt) are live whenever
            // `VELT_SCHEDULER=std` is set. Both WebAssembly targets run the executor in Velt, whose
            // code takes the address of each it uses (`exportedAddress` builds it): not seeded, not
            // reported. A JS host's turns are module exports, seeded above.
            for e in self.hir.c_exports.iter().filter(|e| {
                !e.name.starts_with("std_sched_") && !HOST_TURN_EXPORTS.contains(&e.name.as_str())
            }) {
                self.c_clashes.push(
                    velt_common::Diagnostic::error(
                        format!("`@export(\"{}\")` has no WebAssembly form: a module has no C callers to link with", e.name),
                        e.span,
                    )
                    .with_note("export functions to C on native targets; a WebAssembly host calls into a module through its own exports (#2203)"),
                );
            }
            return;
        }
        // The executor in Velt's own exports wait until the program is known to run on it
        // (`Cx::drain`): one that doesn't leaves them out, as when std is loaded on demand.
        for i in 0..self.hir.c_exports.len() {
            let name = self.hir.c_exports[i].name.as_str();
            if !name.starts_with(SCHED_EXPORTS) && !HOST_TURN_EXPORTS.contains(&name) {
                self.func(Work::CExport(i as u32));
            }
        }
    }

    /// Queues the executor in Velt's own exports (`seed_c_exports`).
    pub(super) fn seed_sched_exports(&mut self) {
        for i in 0..self.hir.c_exports.len() {
            let name = self.hir.c_exports[i].name.as_str();
            if name.starts_with(SCHED_EXPORTS)
                && !BY_ROLE_EXPORTS.contains(&name)
                && !HOST_LEAF_EXPORTS.contains(&name)
            {
                self.func(Work::CExport(i as u32));
            }
        }
    }

    /// The address of the C function of `Program::c_exports[index]` (`exportedAddress`).
    pub(super) fn c_export_address(&mut self, index: u32) -> Operand {
        let f = self.func(Work::CExport(index));
        Operand::Const(Const::Func(f), Ty::Ptr)
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// The C function of `Program::c_exports[index]`.
    pub(super) fn build_c_export(cx: &'c mut Cx<'h>, index: u32) -> Function {
        let hir_prog = cx.hir;
        let e: &hir::CExport = &hir_prog.c_exports[index as usize];
        let hir::Def::Fn(f) = hir_prog.def(e.def) else {
            super::ice("an exported function is not a `Def::Fn`")
        };
        let mut lw = FnLower::bare(cx, vec![]);
        lw.loc = None;
        // On wasm32 `usize` crosses as C's 32-bit `size_t`, as in a call through an address
        // (address_thunk.rs), and is widened for the call.
        let wasm32 = c_call::wasm32(&lw.cx.target);
        let params: Vec<Ty> = (e.params.iter())
            .map(|t| c_call::c_vir(*t, wasm32))
            .collect();
        // One HIR local per parameter, as the call's arguments read them: the value itself.
        // The parameters are the function's first locals.
        let locals: Vec<_> = params.iter().map(|&t| lw.new_local(t, None)).collect();
        let mut info = vec![];
        let mut args = vec![];
        for (k, (ct, p)) in e.params.iter().zip(&f.params).enumerate() {
            let local = locals[k];
            let at = f.span;
            let read = |ty| hir::Expr {
                kind: E::Local(LocalId(k as u32), UseMode::Copy),
                ty,
                span: at,
            };
            if let CTy::Ptr { nullable: false } = ct {
                let v = Operand::Copy(crate::vir::Place::local(local));
                lw.stop_if_null(v, &format!(
                    "C passed NULL for parameter {} of `{}`, a `Ptr<T>`: declare it `Ptr<T> | null`",
                    k + 1,
                    e.name
                ));
            }
            // A `usize`/`isize` C passed as a 32-bit word, widened for the call.
            let local = match c_call::narrow(*ct, wasm32) {
                true => {
                    let wide = lw.cx.ty(p.ty);
                    let v = Operand::Copy(crate::vir::Place::local(local));
                    match lw.rvalue_temp(wide, Rvalue::Cast(v, wide)) {
                        Operand::Copy(w) => w.local,
                        _ => super::ice("a widened argument is not a temporary"),
                    }
                }
                false => local,
            };
            info.push(LInfo::new(Some(local), p.ty, false, false, LState::Init));
            args.push(read(p.ty));
        }
        lw.info = info;
        // A `@repr("transparent")` parameter: C passed its field, which the call's argument is
        // built around (`collect::c_sig`), in a local of the struct's own.
        for (k, p) in f.params.iter().enumerate() {
            if lw.cx.transparent_field(p.ty).is_none() {
                continue;
            }
            let field = Operand::Copy(crate::vir::Place::local(locals[k]));
            let v = lw.wrap_transparent(field, p.ty);
            let vt = lw.cx.ty(p.ty);
            let own = lw.copy_to_temp(v, vt);
            lw.info[k] = LInfo::new(Some(own), p.ty, false, false, LState::Init);
        }
        let call = hir::Expr {
            kind: E::Call {
                callee: hir::Callee::Def(e.def, vec![]),
                args,
            },
            ty: f.ret,
            span: f.span,
        };
        lw.push_scope(super::ScopeKind::Block);
        // Around the call, the executor in Velt holds the tasks a C library's thread starts, so
        // the call's synchronous run comes first (sched.rs `sched_export_call`). Not for its own
        // exports, which are the executor's.
        let held = !e.name.starts_with(SCHED_EXPORTS);
        if held {
            lw.sched_export_call(true);
        }
        let v = lw.expr(&call);
        // A `@repr("transparent")` result returns its field.
        let v = lw.unwrap_transparent(v, f.ret).0;
        let ret = c_call::c_vir(e.ret, wasm32);
        let v = match ret {
            Ty::Unit => unit(),
            _ if c_call::narrow(e.ret, wasm32) => lw.rvalue_temp(ret, Rvalue::Cast(v, ret)),
            _ => lw.rvalue_temp(ret, Rvalue::Use(v)),
        };
        if held {
            lw.sched_export_call(false);
        }
        lw.terminate(Terminator::Return(v));
        lw.scopes.clear();
        let name = e.name.clone();
        let mut func = lw.finish(name, params, ret);
        func.linkage = Linkage::Export;
        func.stops_unwind = true;
        func
    }
}
