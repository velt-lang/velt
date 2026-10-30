//! Member reads and calls on an `any` (phase 1b of #924; hir_encodings.md "any and
//! unknown"): `x.f`, `x[k]`, `x.m(args)` and `x(args)` become `Intrinsic::AnyGet`,
//! `AnyIndex`, `AnyCall` and `AnyApply`, which lowering sends through the descriptor of the
//! value held. Arguments are boxed; results are `any`. On an `unknown` they are errors until
//! it is narrowed, as in TypeScript.

use super::setters::synth;
use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Intrinsic, Lit, Nothing, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// A member of an `any` is read, called or written here: lowering adds the member slots
    /// (`Program::dyn_members`). The standard library's own dynamic operations (`Proxy` and
    /// `Reflect`, std/lang/proxy/index.vlt) don't count: they run only in a program whose own
    /// code constructs a `Proxy` or calls `Reflect`, which counts there (`proxy.rs`).
    pub(crate) fn note_dyn_members(&mut self) {
        if !self.cx.scopes[self.module].is_std {
            self.cx.note_dyn_members();
        }
    }

    /// The receiver `obj` of a dynamic member access (`member`: the member read or called,
    /// `Some(None)` for a computed key, `None` for a call of `obj` itself): `Ok(any type)` for an `any` (and a `Function`
    /// called), `Err(obj)` for anything else. An `unknown`, an `object` and a member of a
    /// `Function` report their error and give `Ok(None)`.
    fn dynamic_receiver(
        &mut self,
        obj: hir::Expr,
        member: Option<Option<&str>>,
    ) -> Result<(hir::Expr, Option<TyId>), hir::Expr> {
        let d = match self.cx.dynamic(obj.ty) {
            None => return Err(obj),
            Some(d) => d,
        };
        match (d, member) {
            (Dynamic::Unknown, _) => {
                self.unknown_use_error(obj.span);
                Ok((obj, None))
            }
            (Dynamic::Object | Dynamic::Function, Some(m)) => {
                self.narrow_dynamic_error(d, m, obj.span);
                Ok((obj, None))
            }
            (Dynamic::Object, None) => {
                self.cx.error(
                    Diagnostic::error("an `object` value is not callable", obj.span)
                        .with_note("narrow it with `typeof x === \"function\"` or convert it with `as Function` first"),
                );
                Ok((obj, None))
            }
            (Dynamic::Any | Dynamic::Function, _) => {
                let mut obj = obj;
                set_place_mode(&mut obj, UseMode::Borrow);
                let any = match d {
                    Dynamic::Function => self.any_ty(),
                    _ => Some(obj.ty),
                };
                self.note_dyn_members();
                Ok((obj, any))
            }
        }
    }

    /// A member `m` of an `object` / `Function` value: TypeScript allows only the members of
    /// `Object.prototype` (and `Function.prototype`), which Velt does not have yet.
    pub(crate) fn narrow_dynamic_error(&mut self, d: Dynamic, m: Option<&str>, span: Span) {
        const OBJECT: &[&str] = &[
            "constructor",
            "hasOwnProperty",
            "isPrototypeOf",
            "propertyIsEnumerable",
            "toLocaleString",
            "toString",
            "valueOf",
        ];
        const FUNCTION: &[&str] = &["apply", "bind", "call", "length", "name"];
        let n = d.name();
        let Some(m) = m else {
            self.cx.error(
                Diagnostic::error(format!("the members of {} `{n}` value are unknown", d.article()), span)
                    .with_note("narrow it with `typeof` or `instanceof` first, or convert it with `as T` (checked at run time)"),
            );
            return;
        };
        let known = OBJECT.contains(&m) || (d == Dynamic::Function && FUNCTION.contains(&m));
        let diag = if known {
            Diagnostic::error(
                format!(
                    "`{m}` of {} `{n}` value is not supported yet (#926)",
                    d.article()
                ),
                span,
            )
            .with_note("convert the value to a typed one first (`x as T`, checked at run time)")
        } else {
            Diagnostic::error(format!("property `{m}` does not exist on type `{n}`"), span)
                .with_note("narrow it with `typeof` or `instanceof` first, or convert it with `as T` (checked at run time)")
        };
        self.cx.error(diag);
    }

    /// Is `t` a symbol (also a `unique symbol`) or `string | symbol`, a key that may name a
    /// record's symbol table (std/lang/record/symbols.vlt)?
    pub(crate) fn symbolic_key_type(&mut self, t: TyId) -> bool {
        let w = self.cx.widened(t);
        matches!(self.cx.ty.kind(w), TyKind::Symbol) || self.string_or_symbol(t)
    }

    /// Is `t` a union of strings, numbers and symbols with a number and a string or symbol
    /// member (`PropertyKey`, `"a" | 1 | symbol`)? A record takes such a key as a
    /// `string | symbol`, a number as its string ([`Self::property_key_of`]).
    pub(crate) fn number_property_key(&mut self, t: TyId) -> bool {
        let Some(members) = self.cx.union_members(t) else {
            return false;
        };
        let (str_, f64_) = (self.cx.ty.str_, self.cx.ty.f64);
        let (mut num, mut other) = (false, false);
        for m in members {
            let w = self.cx.widened(m);
            if w == f64_ || self.cx.ty.is_numeric(w) {
                num = true;
            } else if w == str_ || matches!(self.cx.ty.kind(w), TyKind::Symbol) {
                other = true;
            } else {
                return false;
            }
        }
        num && other
    }

    /// The checked key `h` (a [`Self::number_property_key`]) as a `string | symbol`, as
    /// JavaScript's `ToPropertyKey` makes it: `typeof k === "symbol" ? k : String(k)`, so a
    /// number is its string (`rec[1]` is `rec["1"]`).
    pub(crate) fn property_key_of(&mut self, h: hir::Expr) -> hir::Expr {
        let span = h.span;
        let members = self.cx.union_members(h.ty).unwrap_or_default();
        let symbol = members.iter().any(|&m| {
            let w = self.cx.widened(m);
            matches!(self.cx.ty.kind(w), TyKind::Symbol)
        });
        self.with_temp("<key>", h, span, span, |cx, k| {
            let syn = |kind| synth(kind, span);
            let text = syn(ast::ExprKind::Call {
                callee: Box::new(syn(ast::ExprKind::Ident(ast::Ident {
                    name: "String".into(),
                    span,
                }))),
                type_args: vec![],
                args: vec![k.clone()],
                optional: false,
            });
            if !symbol {
                return cx.expr(&text, None, Want::Move);
            }
            let test = syn(ast::ExprKind::Binary {
                op: ast::BinaryOp::StrictEq,
                lhs: Box::new(syn(ast::ExprKind::Unary {
                    op: ast::UnaryOp::TypeOf,
                    expr: Box::new(k.clone()),
                })),
                rhs: Box::new(syn(ast::ExprKind::Lit(ast::Lit::Str("symbol".into())))),
            });
            let cond = syn(ast::ExprKind::Cond {
                cond: Box::new(test),
                then: Box::new(k.clone()),
                els: Box::new(text),
            });
            cx.expr(&cond, None, Want::Move)
        })
    }

    /// Is `t` a union of strings and symbols (`string | symbol`, a proxy trap's key)? Such a
    /// key goes through `any`: a string names a member, and a symbol known only at run time
    /// stops the program with a `TypeError` (not supported yet), never a wrong member.
    pub(crate) fn string_or_symbol(&mut self, t: TyId) -> bool {
        let Some(members) = self.cx.union_members(t) else {
            return false;
        };
        let str_ = self.cx.ty.str_;
        let mut symbol = false;
        for m in members {
            let w = self.cx.widened(m);
            if matches!(self.cx.ty.kind(w), TyKind::Symbol) {
                symbol = true;
            } else if w != str_ {
                return false;
            }
        }
        symbol
    }

    /// The key `name` as a string literal.
    fn key_lit(&mut self, name: &str, span: Span) -> hir::Expr {
        let ty = self.cx.ty.str_;
        self.mk(H::Lit(Lit::Str(name.to_string())), ty, span)
    }

    /// `obj.name` / `obj["name"]` on an `any`: `Err(obj)` when `obj` is not dynamic.
    pub(crate) fn dynamic_get(
        &mut self,
        obj: hir::Expr,
        name: &str,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        // `x.constructor`: the object of the class `x` holds (`ctor_objects.rs`).
        let obj = match name == "constructor" && want != Want::BorrowMut {
            true => match self.dynamic_constructor(obj, span) {
                Ok(h) => return Ok(h),
                Err(obj) => obj,
            },
            false => obj,
        };
        let (obj, any) = self.dynamic_receiver(obj, Some(Some(name)))?;
        let Some(any) = any else {
            return Ok(self.error_expr(span));
        };
        if want == Want::BorrowMut {
            self.dynamic_write_error(span);
            return Ok(self.error_expr(span));
        }
        let key = self.key_lit(name, span);
        Ok(self.any_op(Intrinsic::AnyGet, vec![obj, key], any, span))
    }

    /// `obj[index]` on an `any`: a string key reads the member it names, a number key the
    /// element, an `any` key either by its `typeof` at run time.
    pub(crate) fn dynamic_index(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        // `o[p as keyof T]` reads the key `p` holds: the assertion converts nothing, and an
        // `any` takes any key.
        let index = match &index.kind {
            ast::ExprKind::Cast { expr, ty }
                if matches!(ty.kind, ast::TypeExprKind::KeyOf(_))
                    && self.cx.dynamic(obj.ty).is_some() =>
            {
                &**expr
            }
            _ => index,
        };
        let key = match &index.kind {
            ast::ExprKind::Lit(ast::Lit::Str(k)) => Some(k.as_str()),
            _ => None,
        };
        let (obj, any) = self.dynamic_receiver(obj, Some(key))?;
        // A symbol constant (`o[KEY]`, `o[Symbol.iterator]`) names the member it keys.
        if let Some(name) = self.symbol_key_of(index) {
            let Some(any) = any else {
                return Ok(self.error_expr(span));
            };
            if want == Want::BorrowMut {
                self.dynamic_write_error(span);
                return Ok(self.error_expr(span));
            }
            let key = self.key_lit(&name, index.span);
            if !self.uses_proxy() {
                return Ok(self.any_op(Intrinsic::AnyGet, vec![obj, key], any, span));
            }
            // A proxy's `get` trap sees the symbol itself (`__anyGetSym`).
            let sym = self.expr(index, None, Want::Borrow);
            return Ok(self.prelude_hir_call("__anyGetSym", vec![obj, key, sym], any, span));
        }
        let k = self.expr(index, None, Want::Borrow);
        let Some(any) = any else {
            return Ok(self.error_expr(span));
        };
        if want == Want::BorrowMut {
            self.dynamic_write_error(span);
            return Ok(self.error_expr(span));
        }
        if self.cx.ty.is_bottom(k.ty) {
            return Ok(self.error_expr(span));
        }
        let str_ = self.cx.ty.str_;
        let f64_ = self.cx.ty.f64;
        let kt = self.cx.widened(k.ty);
        if self.cx.dynamic(k.ty) == Some(Dynamic::Any) || kt == str_ {
            let k = if kt == str_ { self.coerce(k, str_) } else { k };
            return Ok(self.any_op(Intrinsic::AnyGet, vec![obj, k], any, span));
        }
        if self.cx.ty.is_numeric(kt) {
            let k = self.coerce_number(k, f64_);
            return Ok(self.any_op(Intrinsic::AnyIndex, vec![obj, k], any, span));
        }
        // A plain `symbol` key needs `Proxy`'s helpers (velt/lang/proxy/ops), which a program
        // loads with `Proxy` or `Reflect`.
        if self.string_or_symbol(k.ty) || (self.uses_proxy() && self.symbolic_key_type(k.ty)) {
            let k = self.coerce(k, any);
            if self.uses_proxy() {
                // A proxy's `get` trap and a record's symbol table see the symbol.
                return Ok(self.prelude_hir_call("__anyGetKey", vec![obj, k], any, span));
            }
            return Ok(self.any_op(Intrinsic::AnyGet, vec![obj, k], any, span));
        }
        // A key of a type parameter's keys (`K extends keyof T`): its string.
        let k = match self.generic_key_string(k, str_) {
            Ok(s) => return Ok(self.any_op(Intrinsic::AnyGet, vec![obj, s], any, span)),
            Err(k) => k,
        };
        let tn = self.cx.display(k.ty);
        self.cx.error(
            Diagnostic::error(
                format!("indexing an `any` with a `{tn}` key is not supported yet (#924)"),
                index.span,
            )
            .with_note("use a `string` or `number` key"),
        );
        Ok(self.error_expr(span))
    }

    /// `recv.name(args)` on an `any`: `Err(recv)` when `recv` is not dynamic.
    pub(crate) fn dynamic_method_call(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if self.cx.dynamic(recv.ty) == Some(Dynamic::Function) {
            if let Some(h) = self.function_call_apply(&recv, prop, args, span) {
                return Ok(h);
            }
        }
        let typed_callbacks = self.narrowed_dynamic_instance(&recv);
        let (recv, any) = self.dynamic_receiver(recv, Some(Some(&prop.name)))?;
        let Some(any) = any else {
            self.check_args_loose(args);
            return Ok(self.error_expr(span));
        };
        // `xs.join(sep)`: an array held is joined as `Array.prototype.join` does (issue #1462).
        if prop.name == "join" && args.len() <= 1 && !self.cx.scopes[self.module].is_std {
            self.note_dyn_members();
            let recv = self.share_if_place(recv);
            return Ok(self.with_temp("<list>", recv, span, span, |lw, v| {
                let undefined = || synth(ast::ExprKind::Lit(ast::Lit::Undefined), span);
                let sep = args.first().cloned().unwrap_or_else(undefined);
                lw.text_helper_call("__anyJoinMethod", vec![v.clone(), sep], span)
            }));
        }
        let key = self.key_lit(&prop.name, prop.span);
        let text = format!("{}.{}", self.callee_text(&recv), prop.name);
        let text = self.key_lit(&text, span);
        let mut all = vec![recv, key, text];
        let outer = self.any_callback;
        let boxed = self.dynamic_args(args, any, typed_callbacks);
        self.any_callback = outer;
        let Some(boxed) = boxed else {
            return Ok(self.error_expr(span));
        };
        all.extend(boxed);
        Ok(self.intrinsic(Intrinsic::AnyCall, all, any, span))
    }

    /// Is `recv` a variable declared `object` or `unknown` that a test narrowed to an instance
    /// of a generic class or to an array (`x instanceof Map`, `Array.isArray(x)`), which it is
    /// read as an `any` for? TypeScript types it `Map<any, any>` / `any[]`, so an arrow passed
    /// to its method takes `any` parameters there (`raw.forEach((v, k) => …)`), where on an
    /// `any` it needs them written.
    fn narrowed_dynamic_instance(&self, recv: &hir::Expr) -> bool {
        // The read of a narrowed `object` / `unknown` is the value it holds, checked (`AnyTo`)
        // of a share of the variable (`downcast_narrowed`). The result of an array method that
        // returns an array (`u.filter(…)` in `u.filter(…).map(…)`) is TypeScript's `any[]`
        // again when its receiver is one.
        let mut inner = recv;
        for _ in 0..16 {
            inner = match &inner.kind {
                H::Downcast(x) => x.as_ref(),
                H::Call {
                    callee: hir::Callee::Intrinsic(Intrinsic::AnyTo | Intrinsic::Share),
                    args,
                } if args.len() == 1 => &args[0],
                H::Block(b) => match &b.value {
                    Some(v) => v.as_ref(),
                    None => return false,
                },
                H::Call {
                    callee: hir::Callee::Intrinsic(Intrinsic::AnyCall),
                    args,
                } => match args.get(1).map(|k| &k.kind) {
                    Some(H::Lit(Lit::Str(name))) if ARRAY_RESULTS.contains(&name.as_str()) => {
                        &args[0]
                    }
                    _ => return false,
                },
                _ => break,
            };
        }
        let H::Local(l, _) = &inner.kind else {
            return false;
        };
        let declared = self.cx.dynamic(self.local_ty(*l));
        matches!(declared, Some(Dynamic::Object | Dynamic::Unknown))
            && (self.narrowed_class(*l)).is_some_and(|t| self.cx.dynamic(t) == Some(Dynamic::Any))
    }

    /// `f.call(thisArg, ...args)` and `f.apply(thisArg, list)` of a `Function` `f`: the call
    /// `f(...args)` when `thisArg` is `null` or `undefined` (or left out). A Velt function value
    /// has no `this` of its own to set (an arrow ignores it in JavaScript too), so another
    /// `thisArg` stops with a `TypeError` naming #926 rather than calling the function without
    /// it. `f` and `thisArg` are evaluated once, in order, before the arguments. `f.bind(thisArg)`
    /// without arguments to fix is a new function forwarding to `f`, under the same rule. `None`
    /// for other members.
    fn function_call_apply(
        &mut self,
        f: &hir::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        if !matches!(prop.name.as_str(), "call" | "apply" | "bind") {
            return None;
        }
        let bind = prop.name == "bind";
        // `f.bind(thisArg, ...args)` in a program with function expressions taking `this`:
        // the member call through `any`, which such a function answers (std/lang/this_fn/index.vlt).
        if bind && args.len() > 1 && self.cx.prelude_adt("__ThisFn").is_some() {
            let any = self.any_ty()?;
            let key = self.key_lit(&prop.name, prop.span);
            let text = format!("{}.{}", self.callee_text(f), prop.name);
            let text = self.key_lit(&text, span);
            let mut all = vec![f.clone(), key, text];
            all.extend(self.dynamic_args(args, any, false)?);
            return Some(self.intrinsic(Intrinsic::AnyCall, all, any, span));
        }
        let (this, rest) = match args.split_first() {
            Some((this, _)) if matches!(this.kind, ast::ExprKind::Spread(_)) => return None,
            Some((this, rest)) => (Some(this), rest),
            None => (None, args),
        };
        // `f.apply(thisArg, null)` / `undefined`: no arguments, as in JavaScript.
        let no_list = |l: &ast::Expr| {
            matches!(
                l.kind,
                ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined)
            )
        };
        let rest = match (prop.name.as_str(), rest) {
            ("apply", [l]) if no_list(l) => &[],
            _ => rest,
        };
        let given = rest;
        let rest = match (prop.name.as_str(), rest) {
            ("call", _) => rest.to_vec(),
            ("apply" | "bind", []) => vec![],
            ("apply", [list]) => {
                let spread = ast::ExprKind::Spread(Box::new(list.clone()));
                vec![super::setters::synth(spread, list.span)]
            }
            _ => return None,
        };
        let any = self.any_ty()?;
        // `f.bind(thisArg)` without arguments to fix: a new function forwarding to `f`, an
        // `any` (`fn_bind.rs`).
        let bound = |s: &mut Self, f: hir::Expr| s.bound_dynamic(f, any, span);
        let Some(this) = this else {
            // `f.call()` / `f.apply()` / `f.bind()`: `this` is `undefined`.
            return match bind {
                true => Some(bound(self, f.clone())),
                false => self.dynamic_apply(f.clone(), &rest, span).ok(),
            };
        };
        let mut stmts = vec![];
        let f = self.hoist_dynamic(f.clone(), &mut stmts);
        let held = f.clone();
        let t = self.expr(this, Some(any), Want::Borrow);
        if self.cx.ty.is_bottom(t.ty) && t.ty != self.cx.ty.never {
            return Some(t);
        }
        let t = self.share_if_place(t);
        let t = self.coerce(t, any);
        let t = self.hoist_dynamic(t, &mut stmts);
        let call = match bind {
            true => bound(self, f),
            false => self.dynamic_apply(f, &rest, span).ok()?,
        };
        let null = self.dynamic_nothing(Nothing::Null, any, span);
        let b = self.cx.ty.bool_;
        let nullish = self.intrinsic(Intrinsic::AnyLooseEq, vec![t.clone(), null], b, span);
        let msg = format!(
            "TypeError: `{}` with a `this` other than `null` or `undefined` is not supported yet (#926): a function value has no `this`",
            prop.name
        );
        let msg = self.key_lit(&msg, span);
        let never = self.cx.ty.never;
        let stop = match self.cx.prelude_adt("__ThisFn") {
            // A program with function expressions taking `this` (std/lang/this_fn/index.vlt): the member
            // call `f.call(thisArg, ...)` through `any`, which such a function answers.
            Some(_) => {
                let key = self.key_lit(&prop.name, prop.span);
                let text =
                    self.key_lit(&format!("{}.{}", self.callee_text(&held), prop.name), span);
                let boxed = self.dynamic_args(given, any, false)?;
                let mut all = vec![held, key, text, t.clone()];
                all.extend(boxed);
                self.intrinsic(Intrinsic::AnyCall, all, any, span)
            }
            None => self.intrinsic(Intrinsic::Panic, vec![msg], never, span),
        };
        let kind = H::If {
            cond: Box::new(nullish),
            then: Box::new(call),
            els: Box::new(stop),
        };
        let pick = self.mk(kind, any, span);
        Some(self.with_temps_value(stmts, pick))
    }

    /// `f(args)` where `f` is an `any`: `Err(f)` when it is not dynamic.
    pub(crate) fn dynamic_apply(
        &mut self,
        f: hir::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let (f, any) = self.dynamic_receiver(f, None)?;
        let Some(any) = any else {
            self.check_args_loose(args);
            return Ok(self.error_expr(span));
        };
        let text = self.callee_text(&f);
        let text = self.key_lit(&text, span);
        let mut all = vec![f, text];
        let Some(boxed) = self.dynamic_args(args, any, false) else {
            return Ok(self.error_expr(span));
        };
        all.extend(boxed);
        Ok(self.intrinsic(Intrinsic::AnyApply, all, any, span))
    }

    /// The arguments of a dynamic call, each boxed as an `any` (moved into the call). With a
    /// spread (`f(a, ...xs)`), the arguments are one `any[]` built as the array literal
    /// `[a, ...xs]`, whose elements the call takes (hir_encodings.md "any and unknown").
    fn dynamic_args(
        &mut self,
        args: &[ast::Expr],
        any: TyId,
        typed_callbacks: bool,
    ) -> Option<Vec<hir::Expr>> {
        if args
            .iter()
            .any(|a| matches!(a.kind, ast::ExprKind::Spread(_)))
        {
            let span = args[0].span.to(args[args.len() - 1].span);
            let list = super::setters::synth(ast::ExprKind::Array(args.to_vec()), span);
            let arr = self.cx.ty.array(any);
            let h = self.expr(&list, Some(arr), Want::Move);
            if self.cx.ty.is_bottom(h.ty) {
                return None;
            }
            return Some(vec![self.coerce(h, arr)]);
        }
        let mut out = vec![];
        for a in args {
            if typed_callbacks && matches!(a.kind, ast::ExprKind::Arrow { .. }) {
                self.any_callback = Some(a.span);
            }
            let h = self.expr(a, Some(any), Want::Move);
            out.push(self.coerce(h, any));
        }
        Some(out)
    }

    /// How a `TypeError` names the callee `h` (`f`, `o.f`), as Node does.
    fn callee_text(&self, h: &hir::Expr) -> String {
        match &h.kind {
            H::Local(l, _) => self.f.locals[l.0 as usize].name.clone(),
            H::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::AnyGet),
                args,
            } => match args.as_slice() {
                [o, hir::Expr {
                    kind: H::Lit(Lit::Str(k)),
                    ..
                }] => {
                    format!("{}.{k}", self.callee_text(o))
                }
                _ => "the value".to_string(),
            },
            _ => "the value".to_string(),
        }
    }

    /// `s?.<rest>` on an `any` `s` bound to `l` (`mode`), the rest `r` checked on it: the whole
    /// chain is `undefined` when `s` holds `null` or `undefined`, as in JavaScript.
    pub(crate) fn dynamic_chain_match(
        &mut self,
        s: hir::Expr,
        l: hir::LocalId,
        mode: UseMode,
        r: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        if self.cx.ty.is_bottom(r.ty) {
            return r;
        }
        let r = self.coerce(r, any);
        let held = self.mk(H::Local(l, UseMode::Borrow), s.ty, span);
        let null = self.dynamic_nothing(Nothing::Null, any, span);
        let b = self.cx.ty.bool_;
        let nullish = self.intrinsic(Intrinsic::AnyLooseEq, vec![held, null], b, span);
        let undefined = self.dynamic_nothing(Nothing::Undefined, any, span);
        let body = self.mk(
            H::If {
                cond: Box::new(nullish),
                then: Box::new(undefined),
                els: Box::new(r),
            },
            any,
            span,
        );
        let pat = self.pat(hir::PatKind::Binding(l, mode), s.ty, span);
        let arms = vec![hir::Arm {
            pat,
            guard: None,
            body,
        }];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, any, span)
    }

    /// The number value `h` of a numeric type as an `f64` (a key of `AnyIndex`).
    pub(super) fn coerce_number(&mut self, h: hir::Expr, f64_: TyId) -> hir::Expr {
        if h.ty == f64_ {
            return h;
        }
        let span = h.span;
        if let H::Lit(Lit::Int(n)) = h.kind {
            return self.mk(H::Lit(Lit::Float(n as f64)), f64_, span);
        }
        let base = self.cx.widened(h.ty);
        let h = self.coerce(h, base);
        if h.ty == f64_ {
            return h;
        }
        self.mk(hir::ExprKind::Cast(Box::new(h)), f64_, span)
    }
}

/// The array methods whose result is an array of the same kind (`any[]` on an `any[]`).
const ARRAY_RESULTS: &[&str] = &[
    "concat",
    "copyWithin",
    "fill",
    "filter",
    "flat",
    "flatMap",
    "map",
    "reverse",
    "slice",
    "sort",
    "splice",
    "toReversed",
    "toSorted",
    "toSpliced",
    "with",
];
