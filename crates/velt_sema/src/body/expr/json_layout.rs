//! `JSON.stringify(value, replacer, space)`: `JSON.stringify(value)` (unchanged, generated for
//! the value's type) laid out again by the prelude's `jsonRelayout` with JS's property list (an
//! array replacer of strings or numbers, or `null`) and gap (`space`, a number or a string).
//! `JSON.stringify(value)` alone never comes here.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, Callee, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `JSON.stringify(value, replacer, space?)` on the prelude's `JSON`, or the error for
    /// `JSON.parse(text, reviver)` (`json_parse_reviver`); `None` for any other call.
    pub(super) fn json_stringify_layout(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let ast::ExprKind::Ident(o) = &object.kind else {
            return None;
        };
        // `JSON.stringify` of a literal with accessors: of it as an `any` (literal_iface.rs).
        if let Some(h) = self.json_literal_iface(callee, type_args, args, None, span) {
            return Some(h);
        }
        if !matches!(prop.name.as_str(), "stringify" | "parse")
            || args.len() < 2
            || o.name != "JSON"
        {
            return None;
        }
        if self.is_local_name("JSON") {
            return None;
        }
        let json = self.cx.prelude_adt("JSON")?;
        if !matches!(self.cx.lookup_item_at(self.module, "JSON", o.span), Some(Item::Def(d)) if d == json)
        {
            return None;
        }
        if prop.name == "parse" {
            return self.json_parse_reviver(&args[1], span);
        }
        if args.len() > 3 {
            self.arg_count_error("`JSON.stringify`", 1, 3, args.len(), span);
            self.check_args_loose(args);
            return Some(self.error_expr(span));
        }
        let text = self.call(callee, type_args, &args[..1], false, None, span);
        let (text, keys, filter) = match self.json_replacer_fn(&args[1]) {
            // A replacer function rewrites the text first; then only `space` lays it out.
            Ok(f) => {
                let str_ = self.cx.ty.str_;
                let text = self.json_helper_call("jsonReplaceWith", vec![text, f], str_, span);
                (text, self.str_lit("", span), false)
            }
            Err((keys, filter)) => (text, keys, filter),
        };
        let (helper, gap) = match args.get(2) {
            Some(a) => self.json_gap(a),
            None => ("jsonRelayout", self.str_lit("", span)),
        };
        let b = self.cx.ty.bool_;
        let filter = self.mk(H::Lit(hir::Lit::Bool(filter)), b, span);
        let str_ = self.cx.ty.str_;
        Some(self.json_helper_call(helper, vec![text, keys, filter, gap], str_, span))
    }

    /// The replacer, checked once: a function (an arrow or function expression, or a function
    /// value) is checked as the prelude's `JsonReplacer`,
    /// `(key: string, value: JsonValue) => JsonValue | undefined`, and is returned as `Ok`; an
    /// array of keys or `null` gives `Err` with what `json_replacer` makes of it.
    fn json_replacer_fn(&mut self, a: &ast::Expr) -> Result<hir::Expr, (hir::Expr, bool)> {
        let helper = self.cx.prelude_fn("jsonReplaceWith");
        let pty = helper.and_then(|d| self.cx.try_fn(d)?.params.get(1).map(|p| p.ty));
        // An arrow or function expression must write its value parameter `JsonValue`
        // (`json_callbacks`); its other parameters take their types from `JsonReplacer`.
        match self.json_callback_typed(a, "`JSON.stringify`") {
            Some(false) => return Ok(self.error_expr(a.span)),
            Some(true) if pty.is_some() => {
                let pty = pty.expect("ICE: checked");
                let h = self.expr(a, Some(pty), Want::Borrow);
                return Ok(self.coerce(h, pty));
            }
            _ => {}
        }
        if is_null(a) {
            return Err((self.str_lit("", a.span), false));
        }
        let h = self.expr(a, None, Want::Borrow);
        if let (TyKind::FnPtr { .. } | TyKind::Closure(_), Some(pty)) = (self.cx.ty.kind(h.ty), pty)
        {
            // Any other function is a `JsonReplacer` as a whole, or the error below.
            match self.try_coerce(h, pty) {
                Ok(h) => return Ok(h),
                Err(h) => return Err(self.json_replacer(h, a.span)),
            }
        }
        Err(self.json_replacer(h, a.span))
    }

    /// The replacer `h` (checked; not a function, not `null`): its keys' JSON text and whether
    /// it filters.
    fn json_replacer(&mut self, h: hir::Expr, span: Span) -> (hir::Expr, bool) {
        let str_ = self.cx.ty.str_;
        let elem = self.cx.ty.array_elem(h.ty).map(|e| self.cx.widened(e));
        let helper = match elem {
            Some(e) if e == str_ => Some("jsonKeys"),
            Some(e) if self.cx.ty.is_float(e) => Some("jsonNumberKeys"),
            _ => None,
        };
        // `[]` (a `never[]`): no key is written.
        if elem.is_some_and(|e| self.cx.ty.is_bottom(e)) {
            return (self.str_lit("[]", span), true);
        }
        if let Some(f) = helper {
            return (self.json_helper_call(f, vec![h], str_, span), true);
        }
        if h.ty != self.cx.ty.error {
            let note = match self.cx.ty.kind(h.ty) {
                TyKind::FnPtr { .. } | TyKind::Closure(_) => {
                    "a replacer function takes `(key: string, value: JsonValue)` and returns `JsonValue | undefined`"
                }
                _ => "pass an array of the keys to write (strings or numbers), or `null`",
            };
            let found = self.cx.display(h.ty);
            self.cx.error(
                Diagnostic::error(
                    format!("the replacer of `JSON.stringify` cannot be a `{found}`"),
                    span,
                )
                .with_note(note),
            );
        }
        (self.error_expr(span), true)
    }

    /// The prelude helper that lays the text out for `space`, and its last argument: the gap of
    /// a string (`null`: none) for `jsonRelayout`, or the number for `jsonRelayoutNumber`.
    fn json_gap(&mut self, a: &ast::Expr) -> (&'static str, hir::Expr) {
        let span = a.span;
        if is_null(a) {
            return ("jsonRelayout", self.str_lit("", span));
        }
        let f64 = self.cx.ty.f64;
        let h = self.expr(a, Some(f64), Want::Borrow);
        let h = self.unbrand(h);
        let t = self.cx.widened(h.ty);
        let str_ = self.cx.ty.str_;
        if t == str_ {
            let h = self.coerce(h, str_);
            let gap = self.json_helper_call("jsonGapOfString", vec![h], str_, span);
            return ("jsonRelayout", gap);
        }
        if self.cx.ty.is_numeric(t) {
            let h = match self.try_coerce(h, f64) {
                Ok(h) => h,
                Err(h) => self.mk(H::Cast(Box::new(h)), f64, span),
            };
            return ("jsonRelayoutNumber", h);
        }
        if t != self.cx.ty.error && !self.cx.ty.is_bottom(t) {
            let found = self.cx.display(h.ty);
            self.cx.err(
                format!("the `space` of `JSON.stringify` is a number or a string, not a `{found}`"),
                span,
            );
        }
        ("jsonRelayout", self.error_expr(span))
    }

    /// A call of the prelude's helper `name` (a function the compiler calls itself).
    fn json_helper_call(
        &mut self,
        name: &str,
        args: Vec<hir::Expr>,
        ty: TyId,
        span: Span,
    ) -> hir::Expr {
        let Some(d) = self.cx.prelude_fn(name) else {
            self.cx.err(
                "`JSON.stringify` with a replacer or `space` needs the prelude (std/prelude)",
                span,
            );
            return self.error_expr(span);
        };
        // `jsonReplaceWith` throws a `TypeError` for a root replaced by `undefined`.
        self.call_throws(d, &[], ty, span);
        let kind = H::Call {
            callee: Callee::Def(d, vec![]),
            args,
        };
        self.mk(kind, ty, span)
    }
}

fn is_null(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Null) => true,
        ast::ExprKind::Paren(x) => is_null(x),
        _ => false,
    }
}
