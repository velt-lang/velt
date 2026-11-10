//! Calls of C functions (`declare c("lib") function`, docs/internals/design/c-ffi.md): a direct
//! call with the target's C calling convention, the arguments as the declaration types them,
//! and `errno` captured right after it, as for system functions (`lastOsError()` reads it).
//! On `wasm32-wasip1`, std may call the C library `c`, which is wasi-libc
//! (docs/internals/design/wasi-libc.md). Any other WebAssembly call can't be made: an error once
//! lowering is done (system.rs `unavailable_diagnostics`). C constants (`declare c const`) are
//! read here too.

use velt_sema::hir::{self, CTy, DefId, IntTy, TyId, TyKind};

use super::system::Arg;
use super::{cint, ice, unit, FnLower};
use crate::system;
use crate::vir::{self, BinOp, Operand, Place, Rvalue, Ty, STR_AGG};

/// std's hooks that turn a `Ptr` into its address and back (std/ffi/ptr.velt).
pub(super) const PTR_OF: &str = velt_common::std_hook!(ffi_ptr_of);
pub(super) const PTR_OR_NULL: &str = velt_common::std_hook!(ffi_ptr_or_null);
pub(super) const PTR_ADDRESS: &str = velt_common::std_hook!(ffi_ptr_address);
pub(super) const PTR_ADDRESS_OR_ZERO: &str = velt_common::std_hook!(ffi_ptr_address_or_zero);
const STR_BYTES_OF: &str = velt_common::std_hook!(ffi_str_bytes_of);
const C_STRING_BYTES: &str = velt_common::std_hook!(ffi_c_string_bytes);

/// An argument evaluated, before the call reads what it needs of it.
enum CArg {
    /// The C argument itself.
    Ready(Operand),
    /// A buffer: its data pointer is read at the call, after every argument (a later one may
    /// grow an array an earlier one names).
    Buffer(Arg),
    /// A string (a `StrBytes` argument, well-formed): where its bytes are is read at the call.
    Str(Operand),
    /// An object (`Out`, `OutPtr`): the address of its field `name` is taken at the call.
    Field(Place, TyId, &'static str),
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
        if !self.c_callable(x.span, &c.library) {
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
        let wasm = wasm32(&self.cx.target);
        // A variadic function's further arguments, each as what it is (sema checked it is a
        // scalar or a pointer C's `...` takes).
        let types: Vec<CTy> = (c.params.iter().copied())
            .chain(
                args[c.params.len().min(args.len())..]
                    .iter()
                    .map(|a| self.vararg_cty(a.ty)),
            )
            .collect();
        for (k, ((a, t), later)) in args.iter().zip(&types).zip(laters).enumerate() {
            ctys.push(c_vir(*t, wasm));
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
            if let CTy::CString = t {
                // Its bytes (read-only, NUL-terminated), held for the statement. A proxy's `get`
                // trap could give other bytes, without the NUL: refused, as for `Out`'s field.
                let v = self.expr(a);
                let obj = self.operand_place(v, Ty::Ptr);
                self.proxy_guard(&obj);
                let d = self.ffi_hook(C_STRING_BYTES);
                let bytes = self.call_def(d, vec![], vec![Operand::Copy(obj)], &[]);
                let bt = self.cx.fn_def(d).ret;
                let vt = self.cx.ty(bt);
                vals.push(CArg::Buffer(Arg::Value(self.operand_place(bytes, vt), bt)));
                continue;
            }
            if let CTy::Out | CTy::OutPtr = t {
                let v = self.expr(a);
                let obj = self.operand_place(v, Ty::Ptr);
                let field = if *t == CTy::Out { "value" } else { "address" };
                vals.push(CArg::Field(obj, self.sub(a.ty), field));
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
                    let v = self.freeze(v, a.ty);
                    match (narrow(*t, wasm), c_vir(*t, wasm)) {
                        (true, ct) => self.rvalue_temp(ct, Rvalue::Cast(v, ct)),
                        (false, _) => v,
                    }
                }
            };
            // C's default argument promotions, for a variadic argument.
            let v = match (k >= c.params.len(), promoted(*t, wasm)) {
                (true, Some(pt)) => {
                    *ctys
                        .last_mut()
                        .unwrap_or_else(|| ice("a C argument's type")) = pt;
                    self.rvalue_temp(pt, Rvalue::Cast(v, pt))
                }
                _ => v,
            };
            vals.push(CArg::Ready(v));
        }
        let fixed = c.params.len();
        let mut cargs = vec![];
        for v in vals {
            cargs.push(match v {
                CArg::Ready(o) => o,
                CArg::Buffer(arg) => {
                    let p = self.buffer(Some(arg)).0;
                    self.never_null(p)
                }
                CArg::Str(s) => {
                    let p = self.operand_place(s, Ty::Agg(STR_AGG));
                    self.str_data(&p)
                }
                CArg::Field(obj, t, name) => {
                    let (field, _) = self.class_field(&obj, t, name);
                    self.addr(field)
                }
            });
        }
        let cret = c_vir(c.ret, wasm);
        let link = base_symbol(&symbol);
        let decl = (self.cx.locs.as_ref())
            .and_then(|m| m.loc(x.span).map(|l| m.describe(l)))
            .unwrap_or_else(|| link.to_string());
        let shown = shown_signature(&symbol, &c);
        let sig = CSig::of(&c);
        // Another declaration of the symbol (or the compiler's own use) may have fixed its
        // signature with other Velt types: of the same C signature, this call passes its
        // arguments as those types (the same bits), and anything else is a clash.
        let (vparams, vret) = match self.cx.extern_signature(link) {
            None => (ctys[..fixed].to_vec(), cret),
            Some((ps, r, v)) => {
                let (ps, same_vir) = (
                    ps.to_vec(),
                    ps == &ctys[..fixed] && r == cret && v == c.variadic,
                );
                let words = [
                    Ty::Ptr,
                    c_vir(CTy::Int(IntTy::USize), wasm),
                    c_vir(CTy::Int(IntTy::ISize), wasm),
                ];
                let other = self.cx.c_symbols.get(link);
                let compatible = same_vir
                    || match other {
                        Some(o) => o.sig.same(&sig),
                        // The compiler's own use: a word for a pointer it passes.
                        None => {
                            v == c.variadic
                                && ps.len() == fixed
                                && (ps
                                    .iter()
                                    .zip(&c.params)
                                    .zip(&ctys)
                                    .chain([((&r, &c.ret), &cret)]))
                                .all(|((&have, &t), &want)| {
                                    have == want
                                        || (class(t) == CClass::Word && words.contains(&have))
                                })
                        }
                    };
                if !compatible {
                    self.c_clash(def, (link, &shown, &sig), x.span);
                    return match cret {
                        Ty::Unit => unit(),
                        t => Operand::Copy(Place::local(self.temp(t))),
                    };
                }
                (ps, r)
            }
        };
        for (k, want) in vparams.iter().enumerate() {
            if ctys[k] != *want {
                let v = cargs[k].clone();
                cargs[k] = self.rvalue_temp(*want, Rvalue::Cast(v, *want));
            }
        }
        self.cx
            .c_symbols
            .entry(link.to_string())
            .or_insert(super::CDecl {
                span: x.span,
                at: decl.clone(),
                shown,
                sig,
            });
        let id = self.cx.extern_sym_c(link, vparams, vret, c.variadic);
        self.cx.externs[id.0 as usize].c_decl = Some(decl);
        let dest = (vret != Ty::Unit).then(|| self.temp(vret));
        self.call(
            vir::Callee::Extern(id),
            cargs,
            dest.map(Place::local),
            false,
        );
        // Before any drop or other call of the statement: `lastOsError()` reads this. Only for a
        // declaration that asks: it is a call and a store per C call, about 9% of a query that
        // reads a hundred rows from SQLite, whose failures come back as results.
        if c.errno {
            self.capture_errno();
        }
        let Some(d) = dest else {
            return unit();
        };
        // The result as this declaration types it.
        let mut result = Operand::Copy(Place::local(d));
        if vret != cret {
            result = self.rvalue_temp(cret, Rvalue::Cast(result, cret));
        }
        match c.ret {
            CTy::Ptr { nullable } => {
                let addr = self.rvalue_temp(Ty::U64, Rvalue::Cast(result, Ty::U64));
                if !nullable {
                    // C has no non-null pointer type, so the header check can't see this: a NULL
                    // where the declaration says `Ptr<T>` stops the program here, not where the
                    // pointer is used.
                    self.null_result_check(&symbol, addr.clone());
                }
                let hook = if nullable { PTR_OR_NULL } else { PTR_OF };
                let targ = self.ptr_target(ret);
                {
                    let d = self.ffi_hook(hook);
                    self.call_def(d, vec![targ], vec![addr], &[])
                }
            }
            t if narrow(t, wasm) => {
                // `size_t`/`ssize_t` are 32 bits on wasm32; Velt's `usize`/`isize` are 64.
                let wide = c_vir(t, false);
                self.rvalue_temp(wide, Rvalue::Cast(result, wide))
            }
            _ => result,
        }
    }

    /// The error for C declaration `def` (at `at`) of `link` (`shown`, C signature `sig`), whose
    /// symbol another declaration or the compiler's runtime calls with another C signature:
    /// once per declaration, naming the other one and what differs.
    fn c_clash(
        &mut self,
        def: DefId,
        (link, shown, sig): (&str, &str, &CSig),
        at: velt_common::Span,
    ) {
        if !self.cx.c_clashes_reported.insert(def) {
            return;
        }
        let (other, note) = match self.cx.c_symbols.get(link) {
            Some(o) => (format!("`{}` at {}", o.shown, o.at), sig.difference(&o.sig)),
            None => (
                "the signature the compiler's runtime calls it with".to_string(),
                "the compiler's runtime calls this C function itself, with the C types of its own declaration".to_string(),
            ),
        };
        self.cx.c_clashes.push(
            velt_common::Diagnostic::error(
                format!("C function `{link}` has two signatures: `{shown}` here, and {other}"),
                at,
            )
            .with_note(note),
        );
    }

    /// What a variadic argument of type `t` crosses as (sema allowed only these).
    fn vararg_cty(&mut self, t: TyId) -> CTy {
        let t = self.sub(t);
        match self.cx.kind(t) {
            TyKind::Int(i) => CTy::Int(i),
            TyKind::Float(hir::FloatTy::F32) => CTy::F32,
            TyKind::Float(_) => CTy::F64,
            TyKind::Bool => CTy::Bool,
            TyKind::Option(inner, _) if matches!(self.cx.kind(inner), TyKind::Never) => CTy::Null,
            TyKind::Option(..) => CTy::Ptr { nullable: true },
            _ => CTy::Ptr { nullable: false },
        }
    }

    /// Can this target call the C function of `library` declared at `span`? Every native target
    /// can. On `wasm32-wasip1` only std can, and only the C library `c` (wasi-libc, linked into
    /// every program): a package's declarations can't be checked against WASI's headers yet.
    fn c_callable(&self, span: velt_common::Span, library: &str) -> bool {
        if system::target_os(&self.cx.target).is_some() {
            return true;
        }
        wasi(&self.cx.target)
            && library == "c"
            && self.cx.locs.as_ref().is_some_and(|m| m.is_std(span))
    }

    /// A read of the C constant `d` of type `ty` (`declare c const`): its value for this target,
    /// which C headers gave. A target without C headers can't read one: an error once lowering
    /// is done (system.rs `unavailable_diagnostics`).
    pub(super) fn c_const(&mut self, d: DefId, c: hir::CConst, ty: TyId) -> Operand {
        let t = self.sub(ty);
        let vt = self.cx.ty(t);
        match c.value {
            None => {
                self.cx
                    .unavailable
                    .push((d, self.cx.cur_def, self.cx.user_site));
                match vt {
                    Ty::Unit => unit(),
                    vt => Operand::Copy(Place::local(self.temp(vt))),
                }
            }
            Some(hir::CValue::Int(v)) => cint(v, vt),
            Some(hir::CValue::Float(v)) => Operand::Const(vir::Const::Float(v), vt),
            Some(hir::CValue::Bool(v)) => Operand::Const(vir::Const::Bool(v), vt),
            Some(hir::CValue::Address(a)) => {
                let nullable = matches!(c.ty, CTy::Ptr { nullable: true });
                let hook = if nullable { PTR_OR_NULL } else { PTR_OF };
                let targ = self.ptr_target(ty);
                let d = self.ffi_hook(hook);
                self.call_def(d, vec![targ], vec![cint(i128::from(a), Ty::U64)], &[])
            }
        }
    }

    /// Stops the program when `addr` (a `Ptr<T>` result of C function `symbol`) is NULL.
    fn null_result_check(&mut self, symbol: &str, addr: Operand) {
        let null = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, addr, cint(0, Ty::U64)));
        let (bad, ok) = (self.new_block(), self.new_block());
        self.branch(null, bad, ok);
        self.switch_to(bad);
        let name = base_symbol(symbol);
        self.panic_msg(&format!(
            "C function `{name}` returned NULL, but its declaration's result is `Ptr<T>`: declare it `Ptr<T> | null`"
        ));
        self.terminate(vir::Terminator::Unreachable);
        self.switch_to(ok);
    }

    /// `p`, or a dangling address aligned for any element (16) where it is null: an empty array
    /// literal holds a null data pointer, and C's `memcpy`, `memset` or `fwrite` given NULL is
    /// undefined behaviour even for a length of 0. No branch.
    fn never_null(&mut self, p: Operand) -> Operand {
        let p = self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64));
        let null = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, p.clone(), cint(0, Ty::U64)),
        );
        let null = self.rvalue_temp(Ty::U64, Rvalue::Cast(null, Ty::U64));
        let dangling = self.u64_op(BinOp::Shl, null, cint(4, Ty::U64));
        let p = self.u64_op(BinOp::BitOr, p, dangling);
        self.rvalue_temp(Ty::Ptr, Rvalue::Cast(p, Ty::Ptr))
    }

    /// The `T` of `Ptr<T>` or `Ptr<T> | null` (type `t`).
    pub(super) fn ptr_target(&mut self, t: TyId) -> TyId {
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
    pub(super) fn ffi_hook(&mut self, qualified: &'static str) -> DefId {
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

/// `wasm32-wasip1` (`wasm32-wasi` is its older name): WebAssembly with wasi-libc.
pub(super) fn wasi(target: &str) -> bool {
    matches!(target.trim(), "wasm32-wasip1" | "wasm32-wasi")
}

/// A 32-bit WebAssembly target, where C's `size_t` and pointers are 32 bits.
pub(super) fn wasm32(target: &str) -> bool {
    target.trim().starts_with("wasm32")
}

/// Does `t` cross as a narrower integer than Velt's (`usize`/`isize` as 32 bits on wasm32)?
fn narrow(t: CTy, wasm32: bool) -> bool {
    wasm32 && matches!(t, CTy::Int(IntTy::USize | IntTy::ISize))
}

/// The VIR type a C type `t` crosses as; on wasm32 (`wasm32`), `usize`/`isize` are C's 32-bit `size_t`
/// and `ssize_t`.
pub(super) fn c_vir(t: CTy, wasm32: bool) -> Ty {
    match t {
        CTy::Int(IntTy::USize) if wasm32 => Ty::U32,
        CTy::Int(IntTy::ISize) if wasm32 => Ty::I32,
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
        CTy::Ptr { .. }
        | CTy::Null
        | CTy::StrBytes
        | CTy::Buf { .. }
        | CTy::CString
        | CTy::Out
        | CTy::OutPtr => Ty::Ptr,
        CTy::Void => Ty::Unit,
    }
}

/// What a C type is to the C calling convention (c-ffi.md "One C function, several
/// declarations"): two declarations of one symbol whose parameters and results are of one class
/// each pass the same C signature, whatever Velt types they name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CClass {
    /// A pointer of any kind, `size_t` or `ssize_t`: one register class and width on every
    /// target Velt compiles for.
    Word,
    /// Any other integer, by width and signedness (a caller extends a narrow one by its sign).
    Int(IntTy),
    F32,
    F64,
    /// `_Bool`.
    Bool,
    Void,
}

/// The class of C type `t`.
fn class(t: CTy) -> CClass {
    match t {
        CTy::Int(IntTy::USize | IntTy::ISize)
        | CTy::Ptr { .. }
        | CTy::Null
        | CTy::StrBytes
        | CTy::Buf { .. }
        | CTy::CString
        | CTy::Out
        | CTy::OutPtr => CClass::Word,
        CTy::Int(i) => CClass::Int(i),
        CTy::F32 => CClass::F32,
        CTy::F64 => CClass::F64,
        CTy::Bool => CClass::Bool,
        CTy::Void => CClass::Void,
    }
}

/// A declaration's C signature, by class: what two declarations of one symbol must agree on.
#[derive(Clone, Debug)]
pub(crate) struct CSig {
    params: Vec<CClass>,
    ret: CClass,
    variadic: bool,
    /// The Velt types it names (`c_name`), for messages.
    names: Vec<&'static str>,
    ret_name: &'static str,
}

impl CSig {
    fn of(c: &hir::CImport) -> CSig {
        CSig {
            params: c.params.iter().map(|t| class(*t)).collect(),
            ret: class(c.ret),
            variadic: c.variadic,
            names: c.params.iter().map(|t| c_name(*t)).collect(),
            ret_name: c_name(c.ret),
        }
    }

    /// Whether `self` and `other` pass the same C signature.
    fn same(&self, other: &CSig) -> bool {
        self.params == other.params && self.ret == other.ret && self.variadic == other.variadic
    }

    /// What differs between this signature (here) and `other` (there), for the error's note.
    fn difference(&self, other: &CSig) -> String {
        const RULE: &str = "two declarations of one C function must pass the same C types: every pointer, `usize` and `isize` is one machine word, any other integer keeps its width and sign";
        let what = if self.params.len() != other.params.len() {
            format!(
                "it takes {} parameters here and {} there",
                self.params.len(),
                other.params.len()
            )
        } else if self.variadic != other.variadic {
            let (a, b) = if self.variadic {
                ("", " not")
            } else {
                (" not", "")
            };
            format!("it is{a} variadic here and{b} there")
        } else if let Some(k) = (0..self.params.len()).find(|&k| self.params[k] != other.params[k])
        {
            format!(
                "parameter {} is `{}` here and `{}` there",
                k + 1,
                self.names[k],
                other.names[k]
            )
        } else {
            format!(
                "the result is `{}` here and `{}` there",
                self.ret_name, other.ret_name
            )
        };
        format!("{what}: {RULE}")
    }
}

/// The type a variadic argument crossing as `t` is passed as, when C's default argument
/// promotions change it: an integer narrower than `int` and `_Bool` become an `int`, a `float`
/// a `double` (`size_t` stays itself, 32 bits on wasm32).
fn promoted(t: CTy, wasm32: bool) -> Option<Ty> {
    match c_vir(t, wasm32) {
        Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::Bool => Some(Ty::I32),
        Ty::F32 => Some(Ty::F64),
        _ => None,
    }
}

/// How a C function's declaration reads, for messages: `abs$u8(u8): i32`.
fn shown_signature(declared: &str, c: &hir::CImport) -> String {
    let mut ps: Vec<&str> = c.params.iter().map(|t| c_name(*t)).collect();
    if c.variadic {
        ps.push("...");
    }
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
        CTy::CString => "CString",
        CTy::Out => "Out",
        CTy::OutPtr => "OutPtr",
        CTy::Void => "void",
    }
}

/// The link symbol of a declaration named `declared`: without a `$suffix`, so one module can
/// declare one C function with several signatures (`objc_msgSend$rect`).
fn base_symbol(declared: &str) -> &str {
    system::base_name(declared)
}
