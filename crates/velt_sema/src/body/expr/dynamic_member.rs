//! Member reads and calls on an `any` (phase 1b of #924; hir_encodings.md "any and
//! unknown"): `x.f`, `x[k]`, `x.m(args)` and `x(args)` become `Intrinsic::AnyGet`,
//! `AnyIndex`, `AnyCall` and `AnyApply`, which lowering sends through the descriptor of the
//! value held. Arguments are boxed; results are `any`. On an `unknown` they are errors until
//! it is narrowed, as in TypeScript.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Intrinsic, Lit, Nothing, TyId, UseMode};

impl FnCx<'_, '_> {
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
                self.cx.note_dyn_members();
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
                Diagnostic::error(format!("the members of a `{n}` value are unknown"), span)
                    .with_note("narrow it with `typeof` or `instanceof` first, or convert it with `as T` (checked at run time)"),
            );
            return;
        };
        let known = OBJECT.contains(&m) || (d == Dynamic::Function && FUNCTION.contains(&m));
        let diag = if known {
            Diagnostic::error(
                format!("`{m}` of a `{n}` value is not supported yet (#926)"),
                span,
            )
            .with_note("convert the value to a typed one first (`x as T`, checked at run time)")
        } else {
            Diagnostic::error(format!("property `{m}` does not exist on type `{n}`"), span)
                .with_note("narrow it with `typeof` or `instanceof` first, or convert it with `as T` (checked at run time)")
        };
        self.cx.error(diag);
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
        let (obj, any) = self.dynamic_receiver(obj, Some(Some(name)))?;
        let Some(any) = any else {
            return Ok(self.error_expr(span));
        };
        if want == Want::BorrowMut {
            self.dynamic_write_error(span);
            return Ok(self.error_expr(span));
        }
        let key = self.key_lit(name, span);
        Ok(self.intrinsic(Intrinsic::AnyGet, vec![obj, key], any, span))
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
        let key = match &index.kind {
            ast::ExprKind::Lit(ast::Lit::Str(k)) => Some(k.as_str()),
            _ => None,
        };
        let (obj, any) = self.dynamic_receiver(obj, Some(key))?;
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
            return Ok(self.intrinsic(Intrinsic::AnyGet, vec![obj, k], any, span));
        }
        if self.cx.ty.is_numeric(kt) {
            let k = self.coerce_number(k, f64_);
            return Ok(self.intrinsic(Intrinsic::AnyIndex, vec![obj, k], any, span));
        }
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
        let (recv, any) = self.dynamic_receiver(recv, Some(Some(&prop.name)))?;
        let Some(any) = any else {
            self.check_args_loose(args);
            return Ok(self.error_expr(span));
        };
        let key = self.key_lit(&prop.name, prop.span);
        let text = format!("{}.{}", self.callee_text(&recv), prop.name);
        let text = self.key_lit(&text, span);
        let mut all = vec![recv, key, text];
        let Some(boxed) = self.dynamic_args(args, any) else {
            return Ok(self.error_expr(span));
        };
        all.extend(boxed);
        Ok(self.intrinsic(Intrinsic::AnyCall, all, any, span))
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
        let Some(boxed) = self.dynamic_args(args, any) else {
            return Ok(self.error_expr(span));
        };
        all.extend(boxed);
        Ok(self.intrinsic(Intrinsic::AnyApply, all, any, span))
    }

    /// The arguments of a dynamic call, each boxed as an `any` (moved into the call).
    fn dynamic_args(&mut self, args: &[ast::Expr], any: TyId) -> Option<Vec<hir::Expr>> {
        let mut out = vec![];
        for a in args {
            if let ast::ExprKind::Spread(_) = &a.kind {
                self.cx.err(
                    "a spread argument in a call through `any` is not supported yet (#924)",
                    a.span,
                );
                self.check_args_loose(args);
                return None;
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
