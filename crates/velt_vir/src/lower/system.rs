//! Calls of system functions (`declare system function`, crate::system): the declaration's
//! arguments become the C function's, as the table of the target states them. A buffer gives
//! its data pointer and its length (an array's, or a view's: system_views.rs), a path is checked to end with 0, a struct goes through a
//! temporary of the C layout (copied back after the call), guarded integers are checked, and
//! on Apple arm64 the variadic arguments go on the stack (see [`APPLE_VARIADIC_REGS`]).

use velt_sema::hir::{self, DefId, TyId};

use super::operand::proj;
use super::sequence::Later;
use super::{cint, ice, FnLower};
use crate::system::{self, After, Func, Guard, Int, Param, Ret, Struct};
use crate::tls::TlsCall;
use crate::vir::{self, AggLayout, BinOp, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty};

/// Apple's arm64 ABI passes variadic arguments on the stack, in 8-byte slots, where other
/// arguments fill the 8 integer registers first. Calling with exactly 8 integer arguments
/// before them (padding with zeros) puts them where the C function looks, without a variadic
/// call in VIR; Linux and x86-64 pass them like any other argument.
const APPLE_VARIADIC_REGS: usize = 8;

/// `ERROR_IO_PENDING`.
const ERROR_IO_PENDING: i128 = 997;

/// A declaration's argument, lowered.
pub(super) enum Arg {
    Scalar(Operand),
    /// The place of the array's or struct's inline value, and its type.
    Value(Place, TyId),
    /// A view's bytes: their address and length (`u64`).
    Bytes(Operand, Operand),
    /// `xs.subarray(begin, end)` of a `u8[]`: the place of the array's value and its type, and
    /// the two positions evaluated (with their types); the header is read at the call.
    Sub {
        place: Place,
        t: TyId,
        begin: (Operand, TyId),
        end: (Operand, TyId),
    },
}

impl FnLower<'_, '_> {
    /// A call of the system function `def` (an `ExternFnDef` with `system`).
    pub(super) fn system_call(&mut self, def: DefId, args: &[hir::Expr]) -> Operand {
        let hir::Def::ExternFn(x) = self.cx.hir.def(def) else {
            ice("not an extern")
        };
        let ret = self.cx.ty(x.ret);
        let func = match system::match_decl(self.cx.hir, x, &self.cx.target) {
            Ok(Some(f)) if system::callable(f) => f,
            // The target lacks it, or cannot call it yet (`system::callable`); veltc reported
            // every other mismatch. An error, reported once lowering is done
            // (`unavailable_diagnostics`).
            _ => {
                self.cx
                    .unavailable
                    .push((def, self.cx.cur_def, self.cx.user_site));
                for a in args {
                    self.system_arg_effects(a);
                }
                return cint(0, ret);
            }
        };
        // `lastOsError()` reads the errno captured right after the last system call this thread
        // made (set below), not errno itself, so a temporary freed between the failing call and
        // this one cannot change the value, and a callee's failing call is seen by its caller
        // (docs/reference/system-functions.md).
        if let (Ret::Errno, Some((get, _))) = (func.ret, self.errno_tls()) {
            for a in args {
                self.expr(a);
            }
            let v = self.errno_tls_call(get, vec![]);
            return self.rvalue_temp(ret, Rvalue::Cast(v, ret));
        }
        let mut vals = vec![];
        let declared: Vec<&Param> = func.params.iter().filter(|p| p.declared()).collect();
        // What each argument's siblings after it do: a later one may free the buffer an earlier
        // one names (through a module `let` it reassigns, or another reference), so a captured
        // buffer or view is retained across them (`hold_buffer`, `system_view`).
        let laters = self.later_each(args);
        for ((a, p), later) in args.iter().zip(declared).zip(laters) {
            let name = system::param_name(p);
            if let Some(v) = self.system_view(func, name, a, later) {
                vals.push(v);
                continue;
            }
            let v = self.expr(a);
            let t = self.sub(a.ty);
            vals.push(match self.cx.kind(t) {
                hir::TyKind::Int(_) => Arg::Scalar(self.freeze(v, a.ty)),
                _ => {
                    let v = self.hold_buffer(v, a.ty, later);
                    let vt = self.cx.ty(t);
                    Arg::Value(self.operand_place(v, vt), t)
                }
            });
        }
        let mut cargs: Vec<Operand> = vec![];
        let mut ctys: Vec<Ty> = vec![];
        let mut structs: Vec<(Place, TyId, Place, &'static Struct)> = vec![];
        let mut by_param: Vec<Option<Operand>> = vec![];
        let mut vals = vals.into_iter();
        for (i, p) in func.params.iter().enumerate() {
            if i == func.variadic {
                self.variadic_padding(&mut cargs, &mut ctys);
            }
            let variadic = i >= func.variadic;
            let first = cargs.len();
            match *p {
                Param::Null { .. } => push(&mut cargs, &mut ctys, cint(0, Ty::Ptr), Ty::Ptr),
                Param::Int {
                    ty, guard, name, ..
                } => {
                    let Some(Arg::Scalar(v)) = vals.next() else {
                        ice("system call: an integer argument")
                    };
                    self.guard(func, name, &v, ty.vir(), guard);
                    push(&mut cargs, &mut ctys, v, ty.vir());
                }
                Param::Buf { len, .. } => {
                    let (ptr, n) = self.buffer(vals.next());
                    push(&mut cargs, &mut ctys, ptr, Ty::Ptr);
                    let n = match len {
                        Int::U32 => self.clamp_u32(n),
                        _ => n,
                    };
                    push(&mut cargs, &mut ctys, n, len.vir());
                }
                Param::CStr { name } | Param::WStr { name } => {
                    let wide = matches!(p, Param::WStr { .. });
                    let (ptr, n) = self.buffer(vals.next());
                    self.check_terminated(func, name, &ptr, n, wide);
                    push(&mut cargs, &mut ctys, ptr, Ty::Ptr);
                }
                Param::Struct { layout, .. } => {
                    let Some(Arg::Value(place, t)) = vals.next() else {
                        ice("system call: a struct argument")
                    };
                    let tmp = self.c_struct_in(&place, t, layout);
                    let a = self.addr(tmp.clone());
                    structs.push((place, t, tmp, layout));
                    push(&mut cargs, &mut ctys, a, Ty::Ptr);
                }
            }
            by_param.push(cargs.get(first).cloned());
            if variadic {
                // Variadic integers are promoted to 64 bits and pointers passed as integers,
                // so every form of a function (`fcntl`'s) has one signature.
                for k in first..cargs.len() {
                    let t = ctys[k];
                    if t != Ty::I64 {
                        let v = cargs[k].clone();
                        cargs[k] = self.rvalue_temp(Ty::I64, Rvalue::Cast(v, Ty::I64));
                        ctys[k] = Ty::I64;
                    }
                }
            }
        }
        let cret = match func.ret {
            Ret::Int { ty, .. } => ty.vir(),
            Ret::Errno => Ty::Ptr,
        };
        let id = self.cx.extern_sym(func.symbol, ctys, cret, false);
        let dest = self.temp(cret);
        self.call(
            vir::Callee::Extern(id),
            cargs,
            Some(Place::local(dest)),
            false,
        );
        // Capture `errno` into a hidden local straight away, before any drop or other call of
        // the statement runs: a following `lastOsError()` reads this local. `lastOsError`
        // itself (`Ret::Errno`) is read from the C library, so it never overwrites the slot.
        if !matches!(func.ret, Ret::Errno) {
            self.capture_errno();
        }
        if let After::WaitOverlapped { handle, overlapped } = func.after {
            let h = by_param[handle]
                .clone()
                .unwrap_or_else(|| ice("handle argument"));
            let ov = by_param[overlapped]
                .clone()
                .unwrap_or_else(|| ice("overlapped argument"));
            self.wait_overlapped(dest, h, ov);
        }
        for (place, t, tmp, layout) in structs {
            self.c_struct_out(&place, t, &tmp, layout);
        }
        let result = Operand::Copy(Place::local(dest));
        match func.ret {
            Ret::Errno => {
                let at = Place {
                    local: dest,
                    proj: vec![Proj::Deref(Ty::I32)],
                };
                self.rvalue_temp(Ty::I32, Rvalue::Use(Operand::Copy(at)))
            }
            Ret::Int { .. } if cret == ret => result,
            Ret::Int { .. } => self.rvalue_temp(ret, Rvalue::Cast(result, ret)),
        }
    }

    /// The thread-local errno slot (crate::tls): its read and its write, on a target whose
    /// `lastOsError` is an `errno` pointer (Windows reads `GetLastError()` at the call).
    fn errno_tls(&self) -> Option<(TlsCall, TlsCall)> {
        system::overloads(&self.cx.target, "lastOsError").find(|f| matches!(f.ret, Ret::Errno))?;
        let id = crate::tls::stable_id("velt:os::lastOsError:I32");
        let (bits, init) = (32, 0);
        Some((
            TlsCall::Get { id, bits, init },
            TlsCall::Set { id, bits, init },
        ))
    }

    /// Keep the buffer value `v` (of type `ty`) alive across the arguments after it: when one of
    /// them may change memory (`later.memory`) it can free what `v` points to through another
    /// reference (a module `let` it reassigns), so `v` is retained for the statement and released
    /// after the call. A value nothing else reaches, or no later argument can free, is untouched.
    pub(super) fn hold_buffer(
        &mut self,
        v: Operand,
        ty: velt_sema::hir::TyId,
        later: Later,
    ) -> Operand {
        let cty = self.sub(ty);
        if !later.memory || !self.cx.counted(cty) {
            return v;
        }
        let shared = self.share_value(v, cty);
        self.own_value(shared, cty)
    }

    /// Evaluate a system-function argument for its side effects only, on a target that lacks the
    /// function (the call is a compile error, reported once lowering is done). A view argument is
    /// `Intrinsic::Subarray`, which only [`Self::system_view`] knows how to lower; here its
    /// sub-expressions are evaluated (JS order) and the view itself is dropped.
    fn system_arg_effects(&mut self, a: &hir::Expr) {
        if let hir::ExprKind::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::Subarray),
            args,
        } = &a.kind
        {
            for sub in args {
                self.expr(sub);
            }
        } else {
            self.expr(a);
        }
    }

    /// Read `errno` of the calling thread now (`*__error()` / `*__errno_location()`) into the
    /// thread's errno slot, for a later `lastOsError()`, in this function or a caller. Does
    /// nothing on a target whose `lastOsError` is not an `errno` pointer.
    fn capture_errno(&mut self) {
        let Some(errno) =
            system::overloads(&self.cx.target, "lastOsError").find(|f| matches!(f.ret, Ret::Errno))
        else {
            return;
        };
        let Some((_, set)) = self.errno_tls() else {
            return;
        };
        let id = self.cx.extern_sym(errno.symbol, vec![], Ty::Ptr, false);
        let ptr = self.temp(Ty::Ptr);
        self.call(
            vir::Callee::Extern(id),
            vec![],
            Some(Place::local(ptr)),
            false,
        );
        let at = Place {
            local: ptr,
            proj: vec![Proj::Deref(Ty::I32)],
        };
        let v = self.rvalue_temp(Ty::I32, Rvalue::Use(Operand::Copy(at)));
        let v = self.rvalue_temp(Ty::U32, Rvalue::Cast(v, Ty::U32));
        self.errno_tls_call(set, vec![v]);
    }

    /// The call of the thread-local extern `call`, inline in every backend.
    fn errno_tls_call(&mut self, call: TlsCall, args: Vec<Operand>) -> Operand {
        let (params, ret) = call.signature();
        let id = self.cx.extern_sym(&call.symbol(), params, ret, false);
        if ret == Ty::Unit {
            self.call(vir::Callee::Extern(id), args, None, false);
            return super::unit();
        }
        let out = self.temp(ret);
        self.call(
            vir::Callee::Extern(id),
            args,
            Some(Place::local(out)),
            false,
        );
        Operand::Copy(Place::local(out))
    }

    /// Zeros before the variadic arguments on Apple arm64 (`APPLE_VARIADIC_REGS`).
    fn variadic_padding(&mut self, cargs: &mut Vec<Operand>, ctys: &mut Vec<Ty>) {
        let apple_arm = matches!(
            system::target_os(&self.cx.target),
            Some((system::Os::MacOs, true))
        );
        if !apple_arm {
            return;
        }
        while cargs.len() < APPLE_VARIADIC_REGS {
            push(cargs, ctys, cint(0, Ty::I64), Ty::I64);
        }
    }

    /// The data pointer and length (`u64`) of a buffer argument: an array's or a view's.
    fn buffer(&mut self, arg: Option<Arg>) -> (Operand, Operand) {
        match arg {
            Some(Arg::Value(place, t)) => self.header_parts(&place, t),
            Some(Arg::Bytes(ptr, n)) => (ptr, n),
            Some(Arg::Sub {
                place,
                t,
                begin,
                end,
            }) => self.subarray_bytes(&place, t, begin, end),
            _ => ice("system call: a buffer argument"),
        }
    }

    /// `min(n, u32::MAX)` as a `u32` (a Windows length: a shorter read or write is allowed).
    fn clamp_u32(&mut self, n: Operand) -> Operand {
        let max = cint(i128::from(u32::MAX), Ty::U64);
        let big = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Gt, n.clone(), max.clone()));
        let out = self.temp(Ty::U64);
        let (yes, no, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(big, yes, no);
        self.switch_to(yes);
        self.assign(Place::local(out), Rvalue::Use(max));
        self.goto(join);
        self.switch_to(no);
        self.assign(Place::local(out), Rvalue::Use(n));
        self.goto(join);
        self.switch_to(join);
        self.rvalue_temp(
            Ty::U32,
            Rvalue::Cast(Operand::Copy(Place::local(out)), Ty::U32),
        )
    }

    /// Stop the program unless the string at `ptr` (`n` elements) has exactly one 0 element, its
    /// last: the C function's search for the end then stays inside the array and finds the
    /// path the Velt code built (`"a\0../b\0"` would reach C as `"a"`). One scan for the first
    /// 0, which must be element `n - 1`.
    fn check_terminated(&mut self, func: &Func, name: &str, ptr: &Operand, n: Operand, wide: bool) {
        let (stride, et) = if wide { (2, Ty::U16) } else { (1, Ty::U8) };
        let i = Place::local(self.temp(Ty::U64));
        self.assign(i.clone(), Rvalue::Use(cint(0, Ty::U64)));
        let (head, body, next, found) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        let (ok, interior, missing) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(head);
        self.switch_to(head);
        let at_i = Operand::Copy(i.clone());
        let more = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, at_i.clone(), n.clone()));
        self.branch(more, body, missing);
        self.switch_to(body);
        let off = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, at_i.clone(), cint(stride, Ty::U64)),
        );
        let at = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, ptr.clone(), off));
        let Operand::Copy(at) = at else {
            ice("pointer temp")
        };
        let e = self.rvalue_temp(et, Rvalue::Use(Operand::Copy(proj(&at, Proj::Deref(et)))));
        let zero = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, e, cint(0, et)));
        self.branch(zero, found, next);
        self.switch_to(next);
        self.assign(
            i.clone(),
            Rvalue::Binary(BinOp::Add, at_i.clone(), cint(1, Ty::U64)),
        );
        self.goto(head);
        self.switch_to(found);
        let after = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Add, at_i, cint(1, Ty::U64)));
        let last = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, after, n));
        self.branch(last, ok, interior);
        let what = if wide { "element" } else { "byte" };
        self.switch_to(interior);
        self.panic_msg(&format!(
            "system function `{}`: `{name}` has a 0 {what} before its last, where C would end it",
            func.name
        ));
        self.terminate(Terminator::Unreachable);
        self.switch_to(missing);
        self.panic_msg(&format!(
            "system function `{}`: `{name}` must end with a 0 {what}",
            func.name
        ));
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
    }

    /// Stop the program unless the integer `v` passes `guard`.
    fn guard(&mut self, func: &Func, name: &str, v: &Operand, t: Ty, guard: Guard) {
        let (ok, bad) = (self.new_block(), self.new_block());
        let why = match guard {
            Guard::None => return,
            Guard::OneOf(values) => {
                let cases = values.iter().map(|&x| (i128::from(x), ok)).collect();
                self.terminate(Terminator::Switch {
                    value: v.clone(),
                    cases,
                    default: bad,
                });
                "is not one this form of the function takes"
            }
            Guard::NoBits(mask) => {
                let m = self.rvalue_temp(
                    t,
                    Rvalue::Binary(BinOp::BitAnd, v.clone(), cint(i128::from(mask), t)),
                );
                let clear = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, m, cint(0, t)));
                self.branch(clear, ok, bad);
                "has a flag system functions do not allow"
            }
        };
        self.switch_to(bad);
        self.panic_msg(&format!(
            "system function `{}`: `{name}` {why} (docs/reference/system-functions.md)",
            func.name
        ));
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
    }

    /// The aggregate of a C struct's layout (its fields in the table's order, at their offsets).
    fn c_struct_agg(&mut self, layout: &'static Struct) -> vir::AggId {
        let key = layout as *const Struct as usize;
        if let Some(&a) = self.cx.system_aggs.get(&key) {
            return a;
        }
        let a = self.cx.push_agg(AggLayout {
            name: layout.c.to_string(),
            size: layout.size,
            align: layout.align,
            fields: layout
                .fields
                .iter()
                .map(|f| (f.c.vir(), f.offset))
                .collect(),
        });
        self.cx.system_aggs.insert(key, a);
        a
    }

    /// A zeroed temporary of the C layout holding the fields of the Velt struct at `place`.
    fn c_struct_in(&mut self, place: &Place, t: TyId, layout: &'static Struct) -> Place {
        let agg = self.c_struct_agg(layout);
        let tmp = Place::local(self.temp(Ty::Agg(agg)));
        let a = self.addr(tmp.clone());
        self.push_stmt(Stmt::MemSet {
            dst: a,
            byte: cint(0, Ty::U8),
            len: cint(i128::from(layout.size), Ty::U64),
        });
        for (vi, ci) in self.field_pairs(t, layout) {
            let f = layout.fields[ci];
            let src = self.field_place(place, t, vi as u32);
            let v = self.rvalue_temp(f.ty.vir(), Rvalue::Use(Operand::Copy(src)));
            let dst = proj(&tmp, Proj::Field(ci as u32));
            self.assign(dst, Rvalue::Cast(v, f.c.vir()));
        }
        tmp
    }

    /// Copy the fields back from the C temporary into the Velt struct at `place`.
    fn c_struct_out(&mut self, place: &Place, t: TyId, tmp: &Place, layout: &'static Struct) {
        for (vi, ci) in self.field_pairs(t, layout) {
            let f = layout.fields[ci];
            let src = Operand::Copy(proj(tmp, Proj::Field(ci as u32)));
            let dst = self.field_place(place, t, vi as u32);
            self.assign(dst, Rvalue::Cast(src, f.ty.vir()));
        }
    }

    /// (Velt field index, C field index) of each field of the struct type `t`.
    fn field_pairs(&mut self, t: TyId, layout: &Struct) -> Vec<(usize, usize)> {
        let hir::TyKind::Adt(d, _) = self.cx.kind(t) else {
            ice("system call: a struct argument")
        };
        let hir::Def::Adt(a) = self.cx.hir.def(d) else {
            ice("system call: a struct argument")
        };
        a.fields
            .iter()
            .enumerate()
            .map(|(vi, fd)| {
                let ci = layout
                    .fields
                    .iter()
                    .position(|f| f.name == fd.name)
                    .unwrap_or_else(|| ice("system call: a field the table lacks"));
                (vi, ci)
            })
            .collect()
    }

    /// Windows: after a call that returned 0 with `ERROR_IO_PENDING`, wait for the operation
    /// (`GetOverlappedResult(h, ov, &n, TRUE)`), whose result becomes the call's.
    fn wait_overlapped(&mut self, dest: vir::Local, h: Operand, ov: Operand) {
        let (check, wait, done) = (self.new_block(), self.new_block(), self.new_block());
        let failed = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(
                BinOp::Eq,
                Operand::Copy(Place::local(dest)),
                cint(0, Ty::I32),
            ),
        );
        self.branch(failed, check, done);
        self.switch_to(check);
        // As the table's `lastOsError` declares it: one signature per symbol.
        let gle = self.cx.extern_sym("GetLastError", vec![], Ty::I32, false);
        let code = self.temp(Ty::I32);
        self.call(
            vir::Callee::Extern(gle),
            vec![],
            Some(Place::local(code)),
            false,
        );
        let pending = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(
                BinOp::Eq,
                Operand::Copy(Place::local(code)),
                cint(ERROR_IO_PENDING, Ty::I32),
            ),
        );
        self.branch(pending, wait, done);
        self.switch_to(wait);
        let n = Place::local(self.temp(Ty::U32));
        let np = self.addr(n);
        let gor = self.cx.extern_sym(
            "GetOverlappedResult",
            vec![Ty::U64, Ty::Ptr, Ty::Ptr, Ty::I32],
            Ty::I32,
            false,
        );
        let args = vec![h, ov, np, cint(1, Ty::I32)];
        self.call(
            vir::Callee::Extern(gor),
            args,
            Some(Place::local(dest)),
            false,
        );
        self.goto(done);
        self.switch_to(done);
    }
}

fn push(cargs: &mut Vec<Operand>, ctys: &mut Vec<Ty>, v: Operand, t: Ty) {
    cargs.push(v);
    ctys.push(t);
}

impl FnLower<'_, '_> {
    /// A user call (the current `user_site`) of the standard-library function `def`, defined at
    /// `at`: remembered, so a system function the target lacks that it reaches is reported at
    /// this call (`unavailable_diagnostics`).
    pub(super) fn note_std_call(&mut self, def: DefId, at: velt_common::Span) {
        let site = self.cx.user_site;
        let Some(m) = self.cx.locs.as_ref() else {
            return;
        };
        if site != velt_common::Span::DUMMY && m.is_std(at) && !m.is_std(site) {
            self.cx.std_calls.entry(site).or_insert(def);
        }
    }
}

impl super::Cx<'_> {
    /// The errors for calls of system functions the target lacks. One the standard library
    /// calls is reported at the user's call that reached it, as that call of std's function.
    pub(super) fn unavailable_diagnostics(&self) -> Vec<velt_common::Diagnostic> {
        let mut seen_decls = rustc_hash::FxHashSet::default();
        let mut seen_sites = rustc_hash::FxHashSet::default();
        let mut out = vec![];
        for &(d, caller, site) in &self.unavailable {
            let hir::Def::ExternFn(x) = self.hir.def(d) else {
                continue;
            };
            let in_std = self.locs.as_ref().is_some_and(|m| m.is_std(x.span));
            let entry = self.std_calls.get(&site).map(|&e| self.hir.def(e));
            let mut diag = match entry {
                Some(hir::Def::Fn(f)) if in_std => {
                    if !seen_sites.insert(site) {
                        continue;
                    }
                    velt_common::Diagnostic::error(
                        format!("{} is not available on {}", std_name(&f.name), self.target),
                        site,
                    )
                    .with_note(format!(
                        "it calls the system function `{}`, which {} lacks",
                        x.symbol, self.target
                    ))
                }
                _ => {
                    if !seen_decls.insert(d) {
                        continue;
                    }
                    let diag = velt_common::Diagnostic::error(
                        format!(
                            "system function `{}` is not available on {}",
                            x.symbol, self.target
                        ),
                        x.span,
                    );
                    match caller.map(|c| self.hir.def(c)) {
                        Some(hir::Def::Fn(f)) => diag.with_note(format!("`{}` calls it", f.name)),
                        _ => diag,
                    }
                }
            };
            let note = match system::target_os(&self.target) {
                None => "this target has no system functions".to_string(),
                Some(_) => {
                    let mut on: Vec<&str> = system::TARGETS
                        .iter()
                        .copied()
                        .filter(|t| system::available(t, &x.symbol))
                        .collect();
                    on.dedup();
                    match on.is_empty() {
                        true => "no target can call it yet: a Windows function that waits for \
                                 an `OVERLAPPED` is checked but not callable \
                                 (docs/reference/system-functions.md)"
                            .to_string(),
                        false => format!("it is available on {}", on.join(", ")),
                    }
                }
            };
            diag = diag.with_note(note);
            out.push(diag);
        }
        out
    }
}

/// A standard-library function as a program imports it: `std/fs/fd::openSync` is
/// "`openSync` (velt:fs/fd)".
fn std_name(qualified: &str) -> String {
    match qualified.rsplit_once("::") {
        Some((module, name)) => {
            let module = module.strip_prefix("std/").unwrap_or(module);
            format!("`{name}` (velt:{module})")
        }
        None => format!("`{qualified}`"),
    }
}
