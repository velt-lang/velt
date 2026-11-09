//! The functions C calls (`@export("c_name")`, #2203 step 5, docs/internals/design/c-ffi.md
//! "Exported functions"): for each `hir::CExport`, a function with the unmangled symbol `c_name`
//! (`Linkage::Export`) and the C signature sema checked, built whether or not Velt code reaches
//! it. Velt functions already use the target's C calling convention for scalars, so the function
//! only converts what crosses as a `declare c` call does in the other direction (c_call.rs): an
//! address to the `Ptr<T>` it is (std's `ptrOf` / `ptrOrNull`), a `Ptr` result to its address,
//! and calls the exported function as any call does, with its own ABI (a `Ptr` in a register,
//! a result through an out-pointer).
//!
//! A panic that reaches it ends the process: nothing unwinds into C (owner decision (b) on
//! #2649, until #2008). A panic ends the process anyway unless a `catchPanic` poll is up the
//! thread's stack, which only a C function called from Velt can put there; for that case the
//! function has no unwind information (`Function::stops_unwind`), so the unwinder can't pass it
//! and the runtime aborts.
//!
//! WebAssembly has no C callers to link with: an export there is an error (a module export
//! of the function would come with the host's own ABI, not C's).

use velt_sema::hir::{self, CTy, ExprKind as E, LocalId, UseMode};

use super::c_call::{self, PTR_ADDRESS, PTR_ADDRESS_OR_ZERO, PTR_OF, PTR_OR_NULL};
use super::{unit, Cx, FnLower, LInfo, LState, Work};
use crate::vir::{Const, Function, Linkage, Operand, Rvalue, Terminator, Ty};

impl Cx<'_> {
    /// Queues the C function of every `@export` of the program (on WebAssembly, reports each).
    pub(super) fn seed_c_exports(&mut self) {
        if self.hir.c_exports.is_empty() {
            return;
        }
        if self.target.trim().starts_with("wasm") {
            for e in &self.hir.c_exports {
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
        for i in 0..self.hir.c_exports.len() {
            self.func(Work::CExport(i as u32));
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
        let lw_windows = cx.target.contains("windows");
        let mut lw = FnLower::bare(cx, vec![]);
        lw.loc = None;
        let params: Vec<Ty> = e.params.iter().map(|t| c_call::c_vir(*t, false)).collect();
        let usize_ty = lw.cx.intern(hir::TyKind::Int(hir::IntTy::USize));
        // One HIR local per parameter, as the call's arguments read them: the value itself, or
        // a pointer's address as a `usize`.
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
            match ct {
                CTy::Ptr { nullable } => {
                    let addr = lw.new_local(Ty::U64, None);
                    let v = Operand::Copy(crate::vir::Place::local(local));
                    lw.assign(crate::vir::Place::local(addr), Rvalue::Cast(v, Ty::U64));
                    info.push(LInfo::new(Some(addr), usize_ty, false, false, LState::Init));
                    let hook = if *nullable { PTR_OR_NULL } else { PTR_OF };
                    let d = lw.ffi_hook(hook);
                    let target = lw.ptr_target(p.ty);
                    args.push(hir::Expr {
                        kind: E::Call {
                            callee: hir::Callee::Def(d, vec![target]),
                            args: vec![read(usize_ty)],
                        },
                        ty: p.ty,
                        span: at,
                    });
                }
                _ => {
                    info.push(LInfo::new(Some(local), p.ty, false, false, LState::Init));
                    args.push(read(p.ty));
                }
            }
        }
        lw.info = info;
        let mut call = hir::Expr {
            kind: E::Call {
                callee: hir::Callee::Def(e.def, vec![]),
                args,
            },
            ty: f.ret,
            span: f.span,
        };
        if let CTy::Ptr { nullable } = e.ret {
            let hook = if nullable {
                PTR_ADDRESS_OR_ZERO
            } else {
                PTR_ADDRESS
            };
            let d = lw.ffi_hook(hook);
            let target = lw.ptr_target(f.ret);
            call = hir::Expr {
                kind: E::Call {
                    callee: hir::Callee::Def(d, vec![target]),
                    args: vec![call],
                },
                ty: usize_ty,
                span: f.span,
            };
        }
        lw.push_scope(super::ScopeKind::Block);
        let v = lw.expr(&call);
        let ret = c_call::c_vir(e.ret, false);
        let v = match ret {
            Ty::Unit => unit(),
            Ty::Ptr => lw.rvalue_temp(Ty::Ptr, Rvalue::Cast(v, Ty::Ptr)),
            _ => v,
        };
        lw.terminate(Terminator::Return(v));
        lw.scopes.clear();
        let name = e.name.clone();
        let mut func = lw.finish(name, params, ret);
        func.linkage = Linkage::Export;
        // Not on Windows, whose unwinder takes a function without unwind information for a leaf
        // and would misread the stack past it.
        func.stops_unwind = !lw_windows;
        func
    }
}
