//! Builtins: `console.log/info/debug/error/warn`, `process.exit`, `process.memoryUsage`, `panic`, `shared`, `Ok`/`Err`, enum variant
//! constructors, the builtin array/`clone` methods, the M3 task builtins (`spawn`, `sleep`,
//! `yieldNow`, `Promise.all/race/allSettled/any`, `performance.now`, `Date.now`; see `tasks.rs`).
//! The intrinsics std declares (std/lang/intrinsics) are called like functions: `intrinsics.rs`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::Callable;
use super::intrinsics::param;
use super::method::BuiltinMethod;
use crate::body::{FnCx, Want};
use crate::hir::{self, DefId, ExprKind as H, Intrinsic, PassMode, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Call of a builtin function name (not shadowed by a local or item).
    pub(crate) fn builtin_call(
        &mut self,
        id: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let name = id.name.as_str();
        match name {
            super::overload_arg::OVERLOAD_ARG => self.overload_arg(type_args, args, span),
            "panic" => self.simple_intrinsic(Intrinsic::Panic, "`panic`", args, exp, span),
            "shared" => {
                // `shared<i64>(0)`: the written type argument types the value. Without one, an
                // integer literal is an `i64` (`shared(0)` is an atomic counter).
                let exp = match (type_args, args) {
                    ([t], _) => {
                        let t = self.resolve(t);
                        Some(self.cx.ty.intern(TyKind::Shared(t)))
                    }
                    ([], [a]) if exp.is_none() && super::member::untyped_int(a) => {
                        Some(self.cx.ty.intern(TyKind::Shared(self.cx.ty.i64)))
                    }
                    // Where a `shared<T> | null` is expected, the value is a `shared<T>`.
                    _ => exp.map(|t| match self.cx.ty.kind(t) {
                        TyKind::Option(inner, _)
                            if matches!(self.cx.ty.kind(*inner), TyKind::Shared(_)) =>
                        {
                            *inner
                        }
                        _ => t,
                    }),
                };
                let e = self.simple_intrinsic(Intrinsic::SharedNew, "`shared`", args, exp, span);
                if let H::Call { args: a, .. } = &e.kind {
                    if let [x] = a.as_slice() {
                        let (t, at) = (x.ty, x.span);
                        self.no_generator_copy(t, crate::body::GenCopy::Shared, at);
                    }
                }
                e
            }
            "Boolean" => self.boolean_call(args, span),
            "spawn" => self.spawn_call(args, exp, span),
            "attempt" => self.attempt_call(args, span),
            "sleep" => self.simple_intrinsic(Intrinsic::Sleep, "`sleep`", args, exp, span),
            "yieldNow" => self.simple_intrinsic(Intrinsic::YieldNow, "`yieldNow`", args, exp, span),
            "Ok" | "Err" => {
                self.cx.error(
                    Diagnostic::error(format!("`{name}` was removed with `Result`"), id.span)
                        .with_note("throw the error (`throw new NotFound(...)`, callers propagate it automatically), or return a union such as `User | NotFound`"),
                );
                self.check_args_loose(args);
                self.error_expr(span)
            }
            _ => {
                // Calling another environment's global (`requestAnimationFrame(cb)`) throws
                // Node's `ReferenceError` before the arguments are evaluated.
                if let Some(h) = self.browser_global_read(id) {
                    self.check_args_loose(args);
                    return h;
                }
                if !self.unknown_namespace_member(name, id.span)
                    && !self.fn_expr_self_ref(name, id.span)
                {
                    let msg = format!("cannot find `{name}` in this scope");
                    self.cx.not_found(msg, name, id.span);
                }
                self.check_args_loose(args);
                self.error_expr(span)
            }
        }
    }

    pub(super) fn simple_intrinsic(
        &mut self,
        i: Intrinsic,
        what: &str,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let mut c = self.intrinsic_sig(i, span);
        c.what = what.to_string();
        let slots = vec![None; c.slot_names.len()];
        let ck = self.check_call(&c, slots, args, exp, span);
        self.intrinsic(i, ck.args, ck.ret, span)
    }

    /// `console.log(...)`, `console.error(...)`, `process.exit(code)`, `process.memoryUsage()`,
    /// `Promise.all(ps)` (and `race`, `allSettled`, `any`),
    /// `performance.now()`, `Date.now()`.
    pub(crate) fn namespace_builtin(
        &mut self,
        ns: &ast::Ident,
        prop: &ast::Ident,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let simple = match (ns.name.as_str(), prop.name.as_str()) {
            ("process", "exit") => Some((Intrinsic::Exit, "`process.exit`")),
            ("Promise", "all") => Some((Intrinsic::PromiseAll, "`Promise.all`")),
            ("Promise", "race") => Some((Intrinsic::PromiseRace, "`Promise.race`")),
            ("Promise", "allSettled") => {
                return Some(self.prelude_call(
                    "promiseAllSettled",
                    "Promise.allSettled",
                    &[],
                    args,
                    exp,
                    span,
                ));
            }
            ("process", "memoryUsage") => {
                let what = "process.memoryUsage";
                return Some(self.prelude_call("processMemoryUsage", what, &[], args, exp, span));
            }
            ("Promise", "any") => {
                return Some(self.prelude_call("promiseAny", "Promise.any", &[], args, exp, span));
            }
            ("performance", "now") => Some((Intrinsic::PerfNow, "`performance.now`")),
            ("Date", "now") => Some((Intrinsic::DateNow, "`Date.now`")),
            _ => None,
        };
        if let Some((i, what)) = simple {
            return Some(self.simple_intrinsic(i, what, args, exp, span));
        }
        if (ns.name.as_str(), prop.name.as_str()) == ("Array", "from") {
            return Some(self.array_from(args, exp, span));
        }
        let intr = match (ns.name.as_str(), prop.name.as_str()) {
            // Node writes `log`, `info` and `debug` to stdout, `error` and `warn` to stderr.
            ("console", "log" | "info" | "debug") => Intrinsic::Print,
            ("console", "error" | "warn") => Intrinsic::PrintErr,
            (n @ ("console" | "process" | "Promise" | "performance" | "Date"), p) => {
                self.cx
                    .err(format!("no function `{p}` in `{n}`"), prop.span);
                self.check_args_loose(args);
                return Some(self.error_expr(span));
            }
            _ => return None,
        };
        Some(self.print(intr, args, span))
    }

    fn print(&mut self, intr: Intrinsic, args: &[ast::Expr], span: Span) -> hir::Expr {
        let mut out = vec![];
        for a in args {
            let h = self.printed_expr(a);
            let h = self.read_shared(h);
            // A `Date` prints as its ISO string, like Node.
            let h = self.own_to_string(h, "__inspect");
            let h = self.narrowed_for_print(h);
            // A literal with accessors prints as Node shows it (`[Getter]`), through `any`.
            let h = self.literal_iface_as_any(h);
            // A `void` variable (`then((v) => console.log(v))` on a `Promise<void>`) holds
            // `undefined`. A call returning nothing stays an error: Velt's `reverse()` returns
            // nothing where TypeScript's returns the array.
            let h = match h.ty == self.cx.ty.unit && matches!(h.kind, hir::ExprKind::Local(..)) {
                true => {
                    let at = h.span;
                    let undefined =
                        super::setters::synth(ast::ExprKind::Lit(ast::Lit::Undefined), at);
                    let u = self.expr(&undefined, None, Want::Move);
                    let ty = u.ty;
                    let block = hir::Block {
                        stmts: vec![hir::Stmt {
                            kind: hir::StmtKind::Expr(h),
                            span: at,
                        }],
                        value: Some(Box::new(u)),
                        span: at,
                    };
                    self.mk(hir::ExprKind::Block(block), ty, at)
                }
                false => h,
            };
            if !self.printable(h.ty) {
                let tn = self.cx.display(h.ty);
                let mut d =
                    Diagnostic::error(format!("cannot print a value of type `{tn}`"), h.span);
                if let Some(note) = self.in_place_note(a, &h) {
                    d = d.with_note(note);
                }
                if matches!(self.cx.ty.kind(h.ty), TyKind::Simd(_)) {
                    d = d.with_note("print its lanes (`v.lane(0)`), or a mask's `bitmask()`");
                }
                self.cx.error(d);
            }
            out.push(h);
        }
        self.intrinsic(intr, out, self.cx.ty.unit, span)
    }

    /// `Enum.Variant` / `Enum.Variant(args)`.
    pub(crate) fn variant_value(
        &mut self,
        d: DefId,
        prop: &ast::Ident,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let e = self.cx.enum_info(d).expect("ICE: enum");
        let ename = e.name.clone();
        let Some(vi) = e.variants.iter().position(|v| v.name == prop.name) else {
            self.cx.err(
                format!("no variant `{}` in enum `{ename}`", prop.name),
                prop.span,
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let (payload, names, n) = (
            e.variants[vi].payload.clone(),
            e.generics.names.clone(),
            e.generics.len(),
        );
        self.cx
            .rec_ref(prop.span, crate::ide::record::Target::Variant(d, vi as u32));
        let params = payload
            .iter()
            .map(|t| param(*t, PassMode::Owned, span))
            .collect();
        let args_ty: Vec<TyId> = (0..n as u32).map(|i| self.cx.ty.param(i)).collect();
        let ret = self.cx.ty.intern(TyKind::Adt(d, args_ty));
        let c = Callable {
            what: format!("variant `{ename}.{}`", prop.name),
            params,
            ret,
            slot_names: names,
            bounds: vec![vec![]; n],
            constraints: vec![],
            js_numbers: false,
            positions: false,
            js_api: false,
            rest: false,
            defaults: vec![],
        };
        let ck = self.check_call(&c, vec![None; n], args, self.hint(exp), span);
        let kind = H::Variant {
            def: d,
            type_args: ck.type_args,
            variant: vi as u32,
            args: ck.args,
        };
        self.mk(kind, ck.ret, span)
    }

    pub(crate) fn builtin_method_call(
        &mut self,
        mut recv: hir::Expr,
        b: BuiltinMethod,
        prop: &ast::Ident,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let many = args.len() != 1
            || args
                .iter()
                .any(|a| matches!(a.kind, ast::ExprKind::Spread(_)));
        if b == BuiltinMethod::Push && many && self.cx.ty.array_elem(recv.ty).is_some() {
            return self.push_items(recv, args, span);
        }
        if b == BuiltinMethod::ToString && !args.is_empty() && self.cx.ty.is_numeric(recv.ty) {
            return self.number_to_string_radix(recv, args, exp, span);
        }
        let (i, name) = match b {
            BuiltinMethod::Push => (Intrinsic::ArrayPush, "push"),
            BuiltinMethod::Pop => (Intrinsic::ArrayPop, "pop"),
            BuiltinMethod::Clone => (Intrinsic::Clone, "clone"),
            BuiltinMethod::ToString => (Intrinsic::ToString, "toString"),
            BuiltinMethod::SharedAdd => (Intrinsic::SharedAdd, "add"),
            BuiltinMethod::SharedGet => (Intrinsic::SharedGet, "get"),
            BuiltinMethod::SharedSet => (Intrinsic::SharedSet, "set"),
            BuiltinMethod::MutexWith => return self.mutex_with(recv, args, span, false),
            BuiltinMethod::LockRead => return self.mutex_with(recv, args, span, true),
        };
        let mut c = self.intrinsic_sig(i, span);
        c.what = format!("method `{name}`");
        let recv_param = c.params.remove(0);
        let mut slots = vec![None; c.slot_names.len()];
        self.cx.match_ty(recv_param.ty, recv.ty, &mut slots);
        // A promise pushed to an array of a `Promise<T>` written without an error type is
        // stored into that site (`throws::promise_sites`).
        let pushed = self.stored_arg_site(&recv, name, args);
        let outer = std::mem::replace(&mut self.cx.fn_sites.push, pushed);
        let ck = self.check_call(&c, slots, args, exp, span);
        self.cx.fn_sites.push = outer;
        match recv_param.mode {
            PassMode::BorrowMut => self.use_mutably(&mut recv, "call a mutating method on"),
            _ => {
                let m = self.use_mode(recv.ty, Want::Borrow);
                crate::body::places::set_place_mode(&mut recv, m);
            }
        }
        if i == Intrinsic::Clone
            && (self.cx.holds_generator(recv.ty) || self.cx.holds_weak(recv.ty))
        {
            let t = recv.ty;
            self.no_generator_copy(t, crate::body::GenCopy::Clone, prop.span);
        } else if i == Intrinsic::Clone && self.cx.owns_uncopyable(recv.ty) {
            let why = self.cx.uncopyable_why(recv.ty);
            let part = self.cx.uncopyable_part(recv.ty).unwrap_or(recv.ty);
            let note = format!(
                "a copy would share the resource and release it twice; give `{}` a `clone()` method that duplicates the resource (a deep copy then calls it)",
                self.cx.display(part)
            );
            self.cx.error(
                Diagnostic::error(
                    format!("{why}, so it has no automatic `clone()`"),
                    prop.span,
                )
                .with_note(note),
            );
        }
        let mut all = vec![recv];
        all.extend(ck.args);
        self.intrinsic(i, all, ck.ret, span)
    }
    /// `n.toString(radix)` on a number: a call of std's hook `f64ToString`, `i64ToString`
    /// or `u64ToString` (by the receiver's type), with the receiver converted to its parameter.
    fn number_to_string_radix(
        &mut self,
        recv: hir::Expr,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let (name, to) = match self.cx.ty.kind(recv.ty) {
            TyKind::Int(i) if i.is_signed() => {
                (velt_common::std_hook!(i64_to_string), self.cx.ty.i64)
            }
            TyKind::Int(_) => (velt_common::std_hook!(u64_to_string), self.cx.ty.u64),
            _ => (velt_common::std_hook!(f64_to_string), self.cx.ty.f64),
        };
        let Some(crate::ctx::Item::Def(d)) = self.cx.std_def(name) else {
            self.cx
                .err("`toString(radix)` needs the prelude (std/prelude)", span);
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let mut c = self.fn_callable(d, "method `toString`".into(), span);
        c.params.remove(0);
        let slots = vec![None; c.slot_names.len()];
        let ck = self.check_call(&c, slots, args, exp, span);
        let recv_span = recv.span;
        let recv = if recv.ty == to {
            recv
        } else {
            self.mk(H::Cast(Box::new(recv)), to, recv_span)
        };
        let mut call_args = vec![recv];
        call_args.extend(ck.args);
        self.mk(
            H::Call {
                callee: hir::Callee::Def(d, vec![]),
                args: call_args,
            },
            self.cx.ty.str_,
            span,
        )
    }
}
