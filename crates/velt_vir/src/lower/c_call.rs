//! Calls of C functions (`declare c("lib") function`, docs/internals/design/c-ffi.md): a direct
//! call with the target's C calling convention, the arguments as the declaration types them,
//! and `errno` captured right after it, as for system functions (`lastOsError()` reads it). A
//! target without native code (WebAssembly) can't call one: an error once lowering is done
//! (system.rs `unavailable_diagnostics`).

use velt_sema::hir::{self, CTy, DefId, IntTy, TyId, TyKind};

use super::system::Arg;
use super::{cint, ice, unit, FnLower};
use crate::system;
use crate::vir::{self, Operand, Place, Rvalue, Ty, STR_AGG};

/// std's hooks that turn a `Ptr` into its address and back (std/ffi/ptr.vlt).
const PTR_OF: &str = velt_common::std_hook!(ffi_ptr_of);
const PTR_OR_NULL: &str = velt_common::std_hook!(ffi_ptr_or_null);
const PTR_ADDRESS: &str = velt_common::std_hook!(ffi_ptr_address);
const PTR_ADDRESS_OR_ZERO: &str = velt_common::std_hook!(ffi_ptr_address_or_zero);
const STR_BYTES_OF: &str = velt_common::std_hook!(ffi_str_bytes_of);

/// An argument evaluated, before the call reads what it needs of it.
enum CArg {
    /// The C argument itself.
    Ready(Operand),
    /// A buffer: its data pointer is read at the call, after every argument (a later one may
    /// grow an array an earlier one names).
    Buffer(Arg),
    /// A string (a `StrBytes` argument, well-formed): where its bytes are is read at the call.
    Str(Operand),
}

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
        let mut ctys = vec![];
        let mut vals = vec![];
        let laters = self.later_each(args);
        let callee = format!("C function `{}`", base_symbol(&symbol));
        for (k, ((a, t), later)) in args.iter().zip(&c.params).zip(laters).enumerate() {
            ctys.push(c_vir(*t));
            if let CTy::Buf { .. } = t {
                let name = format!("argument {}", k + 1);
                let arg = match self.system_view(&callee, &name, a, later) {
                    Some(v) => v,
                    None => {
                        self.note_held_buffer(a, later);
                        let v = self.expr(a);
                        let v = self.hold_buffer(v, a.ty, later);
                        let t = self.sub(a.ty);
                        let vt = self.cx.ty(t);
                        Arg::Value(self.operand_place(v, vt), t)
                    }
                };
                vals.push(CArg::Buffer(arg));
                continue;
            }
            if let CTy::StrBytes = t {
                let d = self.ffi_hook(STR_BYTES_OF);
                let s = self.call_def(d, vec![], vec![], std::slice::from_ref(a));
                vals.push(CArg::Str(s));
                continue;
            }
            let v = match t {
                CTy::Ptr { nullable } => {
                    let hook = if *nullable {
                        PTR_ADDRESS_OR_ZERO
                    } else {
                        PTR_ADDRESS
                    };
                    let targ = self.ptr_target(a.ty);
                    let addr = {
                        let d = self.ffi_hook(hook);
                        self.call_def(d, vec![targ], vec![], std::slice::from_ref(a))
                    };
                    self.rvalue_temp(Ty::Ptr, Rvalue::Cast(addr, Ty::Ptr))
                }
                CTy::Null => {
                    self.expr(a);
                    cint(0, Ty::Ptr)
                }
                _ => {
                    let v = self.expr(a);
                    self.freeze(v, a.ty)
                }
            };
            vals.push(CArg::Ready(v));
        }
        let mut cargs = vec![];
        for v in vals {
            cargs.push(match v {
                CArg::Ready(o) => o,
                CArg::Buffer(arg) => self.buffer(Some(arg)).0,
                CArg::Str(s) => {
                    let p = self.operand_place(s, Ty::Agg(STR_AGG));
                    self.str_data(&p)
                }
            });
        }
        let cret = c_vir(c.ret);
        let link = base_symbol(&symbol);
        let decl = (self.cx.locs.as_ref())
            .and_then(|m| m.loc(x.span).map(|l| m.describe(l)))
            .unwrap_or_else(|| link.to_string());
        let shown = shown_signature(&symbol, &c);
        let clash = self
            .cx
            .extern_signature(link)
            .is_some_and(|s| s != (&ctys[..], cret));
        if clash {
            // Reported once per declaration, naming the other one (or the runtime's use).
            if self.cx.c_clashes_reported.insert(def) {
                let other = match self.cx.c_symbols.get(link) {
                    Some(o) => format!("`{}` at {}", o.shown, o.at),
                    None => "the signature the compiler's runtime calls it with".to_string(),
                };
                self.cx.c_clashes.push(
                    velt_common::Diagnostic::error(
                        format!("C function `{link}` has two signatures: `{shown}` here, and {other}"),
                        x.span,
                    )
                    .with_note("one symbol has one signature until calls through C function pointers come (#2187 step 4)"),
                );
            }
            return match cret {
                Ty::Unit => unit(),
                t => Operand::Copy(Place::local(self.temp(t))),
            };
        }
        self.cx
            .c_symbols
            .entry(link.to_string())
            .or_insert(super::CDecl {
                span: x.span,
                at: decl.clone(),
                shown,
            });
        let id = self.cx.extern_sym(link, ctys, cret, false);
        self.cx.externs[id.0 as usize].c_decl = Some(decl);
        let dest = (cret != Ty::Unit).then(|| self.temp(cret));
        self.call(
            vir::Callee::Extern(id),
            cargs,
            dest.map(Place::local),
            false,
        );
        // Before any drop or other call of the statement: `lastOsError()` reads this.
        self.capture_errno();
        let Some(d) = dest else {
            return unit();
        };
        let result = Operand::Copy(Place::local(d));
        match c.ret {
            CTy::Ptr { nullable } => {
                let addr = self.rvalue_temp(Ty::U64, Rvalue::Cast(result, Ty::U64));
                let hook = if nullable { PTR_OR_NULL } else { PTR_OF };
                let targ = self.ptr_target(ret);
                {
                    let d = self.ffi_hook(hook);
                    self.call_def(d, vec![targ], vec![addr], &[])
                }
            }
            _ => result,
        }
    }

    /// The `T` of `Ptr<T>` or `Ptr<T> | null` (type `t`).
    fn ptr_target(&mut self, t: TyId) -> TyId {
        let t = self.sub(t);
        let t = match self.cx.kind(t) {
            TyKind::Option(inner, _) => inner,
            _ => t,
        };
        match self.cx.kind(t) {
            TyKind::Adt(_, args) if args.len() == 1 => args[0],
            _ => ice("a C pointer type that is not `Ptr<T>`"),
        }
    }

    /// The definition of std hook `qualified`.
    fn ffi_hook(&mut self, qualified: &'static str) -> DefId {
        if let Some(&d) = self.cx.ffi_hooks.get(qualified) {
            return d;
        }
        let d = self
            .cx
            .prelude_def(qualified)
            .unwrap_or_else(|| ice(format_args!("no `{qualified}` in velt/ffi")));
        self.cx.ffi_hooks.insert(qualified, d);
        d
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
        CTy::Ptr { .. } | CTy::Null | CTy::StrBytes | CTy::Buf { .. } => Ty::Ptr,
        CTy::Void => Ty::Unit,
    }
}

/// How a C function's declaration reads, for messages: `abs$u8(u8): i32`.
fn shown_signature(declared: &str, c: &hir::CImport) -> String {
    let ps: Vec<&str> = c.params.iter().map(|t| c_name(*t)).collect();
    format!("{declared}({}): {}", ps.join(", "), c_name(c.ret))
}

/// A C type as the declaration spells it.
fn c_name(t: CTy) -> &'static str {
    match t {
        CTy::Int(i) => match i {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::ISize => "isize",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::USize => "usize",
        },
        CTy::F32 => "f32",
        CTy::F64 => "f64",
        CTy::Bool => "bool",
        CTy::Ptr { nullable: false } => "Ptr",
        CTy::Ptr { nullable: true } => "Ptr | null",
        CTy::Null => "null",
        CTy::StrBytes => "StrBytes",
        CTy::Buf { readonly: true } => "readonly array",
        CTy::Buf { readonly: false } => "array",
        CTy::Void => "void",
    }
}

/// The link symbol of a declaration named `declared`: without a `$suffix`, so one module can
/// declare one C function with several signatures (`objc_msgSend$rect`).
fn base_symbol(declared: &str) -> &str {
    system::base_name(declared)
}
