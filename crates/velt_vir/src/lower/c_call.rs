//! Calls of C functions (`declare c("lib") function`, docs/internals/design/c-ffi.md): a direct
//! call with the target's C calling convention, the arguments as the declaration types them,
//! and `errno` captured right after it, as for system functions (`lastOsError()` reads it). A
//! target without native code (WebAssembly) can't call one: an error once lowering is done
//! (system.rs `unavailable_diagnostics`).

use velt_sema::hir::{self, CTy, DefId, IntTy};

use super::{ice, unit, FnLower};
use crate::system;
use crate::vir::{self, Operand, Place, Ty};

impl FnLower<'_, '_> {
    /// A call of the C function `def` (an `ExternFnDef` with `c`).
    pub(super) fn c_call(&mut self, def: DefId, args: &[hir::Expr]) -> Operand {
        let hir::Def::ExternFn(x) = self.cx.hir.def(def) else {
            ice("not an extern")
        };
        let Some(c) = x.c.clone() else {
            ice("not a C function")
        };
        let (symbol, ret) = (x.symbol.clone(), x.ret);
        if system::target_os(&self.cx.target).is_none() {
            self.cx
                .unavailable
                .push((def, self.cx.cur_def, self.cx.user_site));
            for a in args {
                self.system_arg_effects(a);
            }
            return match self.cx.ty(ret) {
                Ty::Unit => unit(),
                vt => Operand::Copy(Place::local(self.temp(vt))),
            };
        }
        let mut cargs = vec![];
        let mut ctys = vec![];
        for (a, t) in args.iter().zip(&c.params) {
            let v = self.expr(a);
            cargs.push(self.freeze(v, a.ty));
            ctys.push(c_vir(*t));
        }
        let cret = c_vir(c.ret);
        let link = base_symbol(&symbol);
        if let Some(other) = self
            .cx
            .extern_signature(link)
            .filter(|s| *s != (&ctys[..], cret))
        {
            let other = format!("{other:?}");
            let span = x.span;
            self.cx.c_clashes.push(
                velt_common::Diagnostic::error(
                    format!("C function `{link}` is called with two different signatures"),
                    span,
                )
                .with_note(format!(
                    "this declaration's is {:?} -> {cret:?}; another declaration of the symbol (or the runtime) uses {other}",
                    ctys
                ))
                .with_note("one symbol has one signature until calls through C function pointers come (#2187 step 4)"),
            );
            return match cret {
                Ty::Unit => unit(),
                t => Operand::Copy(Place::local(self.temp(t))),
            };
        }
        self.cx.c_symbols.insert(link.to_string(), x.span);
        let id = self.cx.extern_sym(link, ctys, cret, false);
        let dest = (cret != Ty::Unit).then(|| self.temp(cret));
        self.call(
            vir::Callee::Extern(id),
            cargs,
            dest.map(Place::local),
            false,
        );
        // Before any drop or other call of the statement: `lastOsError()` reads this.
        self.capture_errno();
        match dest {
            Some(d) => Operand::Copy(Place::local(d)),
            None => unit(),
        }
    }
}

/// The VIR type a C type crosses as.
fn c_vir(t: CTy) -> Ty {
    match t {
        CTy::Int(i) => match i {
            IntTy::I8 => Ty::I8,
            IntTy::I16 => Ty::I16,
            IntTy::I32 => Ty::I32,
            IntTy::I64 | IntTy::ISize => Ty::I64,
            IntTy::U8 => Ty::U8,
            IntTy::U16 => Ty::U16,
            IntTy::U32 => Ty::U32,
            IntTy::U64 | IntTy::USize => Ty::U64,
        },
        CTy::F32 => Ty::F32,
        CTy::F64 => Ty::F64,
        CTy::Bool => Ty::Bool,
        CTy::Void => Ty::Unit,
    }
}

/// The link symbol of a declaration named `declared`: without a `$suffix`, so one module can
/// declare one C function with several signatures (`objc_msgSend$rect`).
fn base_symbol(declared: &str) -> &str {
    system::base_name(declared)
}
