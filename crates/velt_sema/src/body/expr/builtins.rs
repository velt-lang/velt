//! Builtins: `console.log/error`, `process.exit`, `process.memoryUsage`, `panic`, `shared`, `Ok`/`Err`, enum variant
//! constructors, the builtin array/`clone` methods, the M3 task builtins (`spawn`, `sleep`,
//! `yieldNow`, `Promise.all/race/allSettled/any`, `performance.now`, `Date.now`; see `tasks.rs`), and
//! `__intrinsic_*` (std modules only).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::Callable;
use super::intrinsics::{intrinsic_named, param};
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
        if let Some(rest) = name.strip_prefix("__intrinsic_") {
            return self.intrinsic_call(id, rest, type_args, args, exp, span);
        }
        match name {
            "panic" => self.simple_intrinsic(Intrinsic::Panic, "`panic`", args, exp, span),
            "shared" => self.simple_intrinsic(Intrinsic::SharedNew, "`shared`", args, exp, span),
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
                if !self.unknown_namespace_member(name, id.span) {
                    self.cx
                        .err(format!("cannot find `{name}` in this scope"), id.span);
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

    fn intrinsic_call(
        &mut self,
        id: &ast::Ident,
        rest: &str,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        if !self.cx.scopes[self.module].is_std {
            self.cx.error(
                Diagnostic::error(format!("`{}` is a compiler intrinsic", id.name), id.span)
                    .with_note("intrinsics can only be called from the standard library (`std/`)"),
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let Some(i) = intrinsic_named(rest) else {
            self.cx
                .err(format!("unknown intrinsic `{}`", id.name), id.span);
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        match i {
            Intrinsic::Print | Intrinsic::PrintErr => return self.print(i, args, span),
            Intrinsic::HttpHandler => return self.http_handler(args, span),
            _ => {}
        }
        let c = self.intrinsic_sig(i, span);
        let mut slots = vec![None; c.slot_names.len()];
        if let [t] = type_args {
            if let Some(s) = slots.first_mut() {
                *s = Some(self.resolve(t));
            }
        }
        let ck = self.check_call(&c, slots, args, exp, span);
        if i == Intrinsic::JsonParse {
            self.json_parse_throws(span);
        }
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
            ("console", "log") => Intrinsic::Print,
            ("console", "error") => Intrinsic::PrintErr,
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
            let h = self.expr(a, None, Want::Borrow);
            // A `Date` prints as its ISO string, like Node.
            let h = self.own_to_string(h, "__inspect");
            if !self.printable(h.ty) {
                let tn = self.cx.display(h.ty);
                self.cx
                    .err(format!("cannot print a value of type `{tn}`"), h.span);
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
            js_numbers: false,
            rest: false,
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
        let (i, name) = match b {
            BuiltinMethod::Push => (Intrinsic::ArrayPush, "push"),
            BuiltinMethod::Pop => (Intrinsic::ArrayPop, "pop"),
            BuiltinMethod::Clone => (Intrinsic::Clone, "clone"),
            BuiltinMethod::ToString => (Intrinsic::ToString, "toString"),
            BuiltinMethod::SharedAdd => (Intrinsic::SharedAdd, "add"),
            BuiltinMethod::SharedGet => (Intrinsic::SharedGet, "get"),
            BuiltinMethod::SharedSet => (Intrinsic::SharedSet, "set"),
            BuiltinMethod::MutexWith => return self.mutex_with(recv, args, span),
        };
        let mut c = self.intrinsic_sig(i, span);
        c.what = format!("method `{name}`");
        let recv_param = c.params.remove(0);
        let mut slots = vec![None; c.slot_names.len()];
        self.cx.match_ty(recv_param.ty, recv.ty, &mut slots);
        let ck = self.check_call(&c, slots, args, exp, span);
        match recv_param.mode {
            PassMode::BorrowMut => self.use_mutably(&mut recv, "call a mutating method on"),
            _ => {
                let m = self.use_mode(recv.ty, Want::Borrow);
                crate::body::places::set_place_mode(&mut recv, m);
            }
        }
        if i == Intrinsic::Clone && matches!(self.cx.ty.kind(recv.ty), TyKind::Promise(..)) {
            self.cx.error(
                Diagnostic::error("a promise cannot be copied", prop.span).with_note(
                    "it runs once and has one owner; pass the promise itself on, or await it and copy the result",
                ),
            );
        } else if i == Intrinsic::Clone && self.cx.owns_uncopyable(recv.ty) {
            let why = self.cx.uncopyable_why(recv.ty);
            let part = self.cx.uncopyable_part(recv.ty).unwrap_or(recv.ty);
            let note = match self.cx.ty.kind(part) {
                TyKind::Promise(..) => "a promise runs once and has one owner; await it and copy the result".to_string(),
                _ => format!(
                    "a copy would share the resource and release it twice; give `{}` a `clone()` method that duplicates the resource (a deep copy then calls it)",
                    self.cx.display(part)
                ),
            };
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
}
