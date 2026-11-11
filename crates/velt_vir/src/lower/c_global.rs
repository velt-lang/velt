//! C variables (`declare c("lib") let name: T;`, docs/internals/design/c-ffi.md "C variables"):
//! a read loads the variable from C memory, through its link symbol, at each use; an assignment
//! stores it there. The symbol is an `Import` static with no contents (`StaticData::c_global`).
//! On Windows every C library is a DLL, whose variables a linked program reaches through the
//! import library's `__imp_<name>` pointer, so the address is loaded from there first; under the
//! JIT (`LowerOptions::jit`) the host resolves the variable's own name to its address, as it does
//! on every other target ([`c_global_symbol`]). A target that
//! can't call the library's functions (WebAssembly) can't reach its variables either: an error
//! once lowering is done, as for a C function.

use velt_sema::hir::{self, CTy, DefId, TyId};

use super::c_call::{c_vir, narrow, wasm32, PTR_ADDRESS, PTR_ADDRESS_OR_ZERO, PTR_OF, PTR_OR_NULL};
use super::{cint, unit, Cx, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, StaticData, StaticId, Ty};

/// The link symbol of C variable `name` on `target`, and whether it holds the variable's address
/// rather than being the variable: a program linked for Windows reaches a DLL's variable through
/// the import library's `__imp_<name>` pointer, which only the linker makes; under the JIT
/// (`jit`) the host resolves `<name>` itself (`GetProcAddress`, `dlsym`).
pub(crate) fn c_global_symbol(name: &str, target: &str, jit: bool) -> (String, bool) {
    match target.contains("-windows") && !jit {
        true => (format!("__imp_{name}"), true),
        false => (name.to_string(), false),
    }
}

impl Cx<'_> {
    /// The static of link symbol `symbol` (aligned to `align`), one per symbol; `pointer`: it
    /// holds the variable's address ([`c_global_symbol`]).
    fn c_global_static(&mut self, symbol: String, align: u32, pointer: bool) -> StaticId {
        if let Some(&id) = self.c_globals.get(&symbol) {
            return id;
        }
        let id = StaticId(self.statics.len() as u32);
        self.statics.push(StaticData {
            bytes: vec![],
            align: if pointer { 8 } else { align },
            symbol: Some(symbol.clone()),
            linkage: vir::Linkage::Import,
            c_global: true,
            ..Default::default()
        });
        self.c_globals.insert(symbol, id);
        id
    }
}

impl FnLower<'_, '_> {
    /// A read of C constant or variable `d`.
    pub(super) fn c_value(&mut self, d: DefId) -> Operand {
        let hir::Def::Global(g) = self.cx.hir.def(d) else {
            super::ice("a C value that is not a global")
        };
        let (ty, c_const, c_global) = (g.ty, g.c_const, g.c_global.clone());
        match (c_const, c_global) {
            (Some(c), _) => self.c_const(d, c, ty),
            (None, Some(c)) => self.c_global_read(d, &c, ty),
            (None, None) => super::ice("a C value that is neither a constant nor a variable"),
        }
    }

    /// The C variable place `e` is, when it is one.
    pub(super) fn c_global_of(&self, e: &hir::Expr) -> Option<(DefId, hir::CGlobal)> {
        let hir::ExprKind::Global(d) = e.kind else {
            return None;
        };
        match self.cx.hir.def(d) {
            hir::Def::Global(g) => g.c_global.clone().map(|c| (d, c)),
            _ => None,
        }
    }

    /// A read of C variable `d` (`c`) of type `ty`: its value in C memory now, as the
    /// declaration types it.
    pub(super) fn c_global_read(&mut self, d: DefId, c: &hir::CGlobal, ty: TyId) -> Operand {
        let t = self.sub(ty);
        let vt = self.cx.ty(t);
        let Some(at) = self.c_global_place(d, c) else {
            return match vt {
                Ty::Unit => unit(),
                vt => Operand::Copy(Place::local(self.temp(vt))),
            };
        };
        let wasm = wasm32(&self.cx.target);
        let raw = self.rvalue_temp(c_vir(c.ty, wasm), Rvalue::Use(Operand::Copy(at)));
        match c.ty {
            CTy::Ptr { nullable } => {
                let addr = self.rvalue_temp(Ty::U64, Rvalue::Cast(raw, Ty::U64));
                if !nullable {
                    self.null_variable_check(&c.symbol, addr.clone());
                }
                let hook = if nullable { PTR_OR_NULL } else { PTR_OF };
                let targ = self.ptr_target(ty);
                let h = self.ffi_hook(hook);
                self.call_def(h, vec![targ], vec![addr], &[])
            }
            t if narrow(t, wasm) => {
                let wide = c_vir(t, false);
                self.rvalue_temp(wide, Rvalue::Cast(raw, wide))
            }
            _ => raw,
        }
    }

    /// `x = value` for C variable `d` (`c`): stored in C memory.
    pub(super) fn c_global_assign(
        &mut self,
        d: DefId,
        c: &hir::CGlobal,
        value: &hir::Expr,
    ) -> Operand {
        let v = self.consume(value);
        self.c_global_store(d, c, v, value.ty);
        unit()
    }

    /// `x op= value` for C variable `d` (`c`, of type `ty`): read, computed, stored.
    pub(super) fn c_global_compound(
        &mut self,
        d: DefId,
        c: &hir::CGlobal,
        (op, ty): (hir::BinOp, TyId),
        value: &hir::Expr,
    ) -> Operand {
        let l = self.c_global_read(d, c, ty);
        let r = self.expr(value);
        let v = self.binop(op, l, r, ty, ty);
        self.c_global_store(d, c, v, ty);
        unit()
    }

    /// Store `v` (of Velt type `ty`) into C variable `d` (`c`).
    fn c_global_store(&mut self, d: DefId, c: &hir::CGlobal, v: Operand, ty: TyId) {
        let wasm = wasm32(&self.cx.target);
        let v = match c.ty {
            CTy::Ptr { nullable } => {
                let hook = if nullable {
                    PTR_ADDRESS_OR_ZERO
                } else {
                    PTR_ADDRESS
                };
                let targ = self.ptr_target(ty);
                let h = self.ffi_hook(hook);
                // The hook borrows its `Ptr` (or option) argument: passed by its address.
                let vt = {
                    let t = self.sub(ty);
                    self.cx.ty(t)
                };
                let arg = match vt {
                    Ty::Agg(_) => {
                        let at = self.operand_place(v, vt);
                        self.addr(at)
                    }
                    _ => v,
                };
                let addr = self.call_def(h, vec![targ], vec![arg], &[]);
                self.rvalue_temp(Ty::Ptr, Rvalue::Cast(addr, Ty::Ptr))
            }
            t if narrow(t, wasm) => {
                let ct = c_vir(t, wasm);
                self.rvalue_temp(ct, Rvalue::Cast(v, ct))
            }
            _ => v,
        };
        if let Some(at) = self.c_global_place(d, c) {
            self.assign(at, Rvalue::Use(v));
        }
    }

    /// Where C variable `d` (`c`) is: its symbol's address, dereferenced as its C type; `None`
    /// (recorded as an error) on a target that can't reach its library.
    fn c_global_place(&mut self, d: DefId, c: &hir::CGlobal) -> Option<Place> {
        let span = match self.cx.hir.def(d) {
            hir::Def::Global(g) => g.span,
            _ => velt_common::Span::DUMMY,
        };
        if !self.c_callable(span, &c.library) {
            self.cx
                .unavailable
                .push((d, self.cx.cur_def, self.cx.user_site));
            return None;
        }
        let ct = c_vir(c.ty, wasm32(&self.cx.target));
        let align = ct.scalar_align().unwrap_or(8);
        let (symbol, pointer) = c_global_symbol(&c.symbol, &self.cx.target, self.cx.jit);
        let id = self.cx.c_global_static(symbol, align, pointer);
        let mut p = self.temp(Ty::Ptr);
        let addr = Operand::Const(vir::Const::Static(id), Ty::Ptr);
        self.assign(Place::local(p), Rvalue::Use(addr));
        if pointer {
            let imp = Place {
                local: p,
                proj: vec![Proj::Deref(Ty::Ptr)],
            };
            let q = self.temp(Ty::Ptr);
            self.assign(Place::local(q), Rvalue::Use(Operand::Copy(imp)));
            p = q;
        }
        Some(Place {
            local: p,
            proj: vec![Proj::Deref(ct)],
        })
    }

    /// Stops the program when `addr` (the value of C variable `symbol`, declared `Ptr<T>`) is
    /// NULL: C has no non-null pointer type, so the header check can't see it.
    fn null_variable_check(&mut self, symbol: &str, addr: Operand) {
        let null = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, addr, cint(0, Ty::U64)));
        let (bad, ok) = (self.new_block(), self.new_block());
        self.branch(null, bad, ok);
        self.switch_to(bad);
        self.panic_msg(&format!(
            "C variable `{symbol}` is NULL, but its declaration's type is `Ptr<T>`: declare it `Ptr<T> | null`"
        ));
        self.terminate(vir::Terminator::Unreachable);
        self.switch_to(ok);
    }
}
