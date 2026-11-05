//! `t.join(sep)` and `t.toString()` on a tuple (issue #1462), as `Array.prototype.join` writes
//! an array (a tuple is one in JavaScript): each element as `String(x)` writes it, `null` and
//! `undefined` as empty text, the receiver evaluated before the separator.

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};

/// `t[i]`.
fn element(t: &ast::Expr, i: usize, span: Span) -> ast::Expr {
    let index = ast::Lit::Int {
        value: i as u128,
        suffix: None,
    };
    let kind = ast::ExprKind::Index {
        object: Box::new(t.clone()),
        index: Box::new(synth(ast::ExprKind::Lit(index), span)),
        optional: false,
        more: Vec::new(),
    };
    synth(kind, span)
}

/// `tupleElements(array, (i) => i === 0 ? t[0] : … : t[n - 1])`, for a tuple of `n` elements, at
/// least one.
fn tuple_elements(t: &ast::Expr, n: usize, array: ast::Expr, span: Span) -> ast::Expr {
    let i = ast::Ident {
        name: "<i>".into(),
        span,
    };
    let mut read = element(t, n - 1, span);
    for k in (0..n - 1).rev() {
        let at = ast::Lit::Int {
            value: k as u128,
            suffix: None,
        };
        let test = binary(
            ast::BinaryOp::StrictEq,
            synth(ast::ExprKind::Ident(i.clone()), span),
            synth(ast::ExprKind::Lit(at), span),
            span,
        );
        let kind = ast::ExprKind::Cond {
            cond: Box::new(test),
            then: Box::new(element(t, k, span)),
            els: Box::new(read),
        };
        read = synth(kind, span);
    }
    let param = ast::ArrowParam {
        name: i,
        pattern: None,
        ty: None,
        default: None,
        optional: false,
        rest: false,
    };
    let arrow = ast::ExprKind::Arrow {
        type_params: vec![],
        params: vec![param],
        ret: None,
        throws: None,
        body: ast::ArrowBody::Expr(Box::new(read)),
        is_async: false,
    };
    let callee = ast::Ident {
        name: velt_common::std_hook!(tuple_elements).into(),
        span,
    };
    let kind = ast::ExprKind::Call {
        callee: Box::new(synth(ast::ExprKind::Ident(callee), span)),
        type_args: vec![],
        args: vec![array, synth(arrow, span)],
        optional: false,
    };
    synth(kind, span)
}

/// Is `e` made of literals, names and operators, which can't write to a tuple?
fn runs_nothing(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(_) | ast::ExprKind::Ident(_) => true,
        ast::ExprKind::Paren(x) | ast::ExprKind::Unary { expr: x, .. } => runs_nothing(x),
        ast::ExprKind::Binary { lhs, rhs, .. } => runs_nothing(lhs) && runs_nothing(rhs),
        ast::ExprKind::Array(xs) => xs.iter().all(runs_nothing),
        ast::ExprKind::Cond { cond, then, els } => {
            runs_nothing(cond) && runs_nothing(then) && runs_nothing(els)
        }
        _ => false,
    }
}

fn binary(op: ast::BinaryOp, lhs: ast::Expr, rhs: ast::Expr, span: Span) -> ast::Expr {
    let kind = ast::ExprKind::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    };
    synth(kind, span)
}

fn string_of(e: ast::Expr, span: Span) -> ast::Expr {
    let callee = ast::Ident {
        name: "String".into(),
        span,
    };
    let kind = ast::ExprKind::Call {
        callee: Box::new(synth(ast::ExprKind::Ident(callee), span)),
        type_args: vec![],
        args: vec![e],
        optional: false,
    };
    synth(kind, span)
}

fn str_lit(s: &str, span: Span) -> ast::Expr {
    synth(ast::ExprKind::Lit(ast::Lit::Str(s.into())), span)
}

impl FnCx<'_, '_> {
    /// A method of arrays that doesn't change its receiver, on a tuple (`t.map(f)`, `t.reduce(f)`),
    /// over the tuple's elements as an array has them: their type is their union, as TypeScript
    /// types a tuple's methods. One that calls back reads each element when it reaches it, after
    /// the callbacks before it ran (`tupleElements`, std/lang/hooks/tuple_elements.vlt); the
    /// others are called on an array of the elements. `Err` gives `recv` back for other receivers
    /// and methods.
    pub(super) fn tuple_array_call(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        (type_args, args): (&[ast::TypeExpr], &[ast::Expr]),
        exp: Option<TyId>,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let elems = match self.cx.ty.kind(recv.ty) {
            TyKind::Tuple(elems) if ARRAY_READERS.contains(&prop.name.as_str()) => elems.len(),
            _ => return Err(recv),
        };
        let recv = self.share_if_place(recv);
        Ok(self.with_temp("<tuple>", recv, span, span, |lw, t| {
            let parts = (0..elems).map(|i| element(t, i, span)).collect();
            let array = synth(ast::ExprKind::Array(parts), span);
            // std code doesn't load the hook (std/prelude/global/tuple_elements.vlt).
            let tuple_elements_hook = velt_common::std_hook!(tuple_elements);
            let loaded = lw.cx.std_def(tuple_elements_hook).is_some();
            let live = match (prop.name.as_str(), args.len()) {
                _ if elems == 0 || !loaded => None,
                ("reduce", 1) => Some("reduceFirst"),
                ("reduceRight", 1) => Some("reduceRightFirst"),
                (name, _) => CALLING_BACK.iter().copied().find(|m| *m == name),
            };
            let Some(live) = live else {
                // What the first argument is expected to be, as for an array: `m.concat([f()])`
                // of a `[number, string]` takes a `(number | string)[]`.
                let first = match prop.name.as_str() {
                    _ if args.first().is_none_or(runs_nothing) => None,
                    "concat" => lw.trial_type(&array),
                    "includes" | "indexOf" | "lastIndexOf" => {
                        (lw.trial_type(&array)).and_then(|a| lw.cx.ty.array_elem(a))
                    }
                    _ => None,
                };
                return lw.args_before_elements(args, first, &mut vec![], |lw, args| {
                    lw.member_call(&array, prop, false, type_args, args, exp, span)
                });
            };
            let method = ast::Ident {
                name: live.into(),
                span: prop.span,
            };
            let elements = tuple_elements(t, elems, array, span);
            lw.member_call(&elements, &method, false, type_args, args, exp, span)
        }))
    }

    /// `body` with `args`, those that may run code evaluated into temporaries first: JavaScript
    /// reads a tuple's elements after a method's arguments (`t.at(f())`, where `f` writes to `t`).
    /// `expected`: what the next one is expected to be.
    fn args_before_elements(
        &mut self,
        args: &[ast::Expr],
        expected: Option<TyId>,
        done: &mut Vec<ast::Expr>,
        body: impl FnOnce(&mut Self, &[ast::Expr]) -> hir::Expr,
    ) -> hir::Expr {
        let Some((a, rest)) = args.split_first() else {
            return body(self, done);
        };
        if runs_nothing(a) || matches!(a.kind, ast::ExprKind::Spread(_)) {
            done.push(a.clone());
            return self.args_before_elements(rest, None, done, body);
        }
        let name = format!("<arg{}>", done.len());
        let h = self.expr(a, expected, Want::Move);
        let h = match expected {
            Some(t) => self.coerce(h, t),
            None => h,
        };
        self.with_temp(&name, h, a.span, a.span, |lw, v| {
            done.push(v.clone());
            lw.args_before_elements(rest, None, done, body)
        })
    }

    /// `t.length` of a tuple: how many elements its type has, `t` evaluated for what it does.
    pub(super) fn tuple_length(&mut self, t: hir::Expr, span: Span) -> hir::Expr {
        let n = match self.cx.ty.kind(t.ty) {
            TyKind::Tuple(elems) => elems.len(),
            _ => 0,
        };
        let n = ast::Lit::Int {
            value: n as u128,
            suffix: None,
        };
        let n = synth(ast::ExprKind::Lit(n), span);
        if crate::body::places::is_place(&t) {
            return self.expr(&n, None, Want::Move);
        }
        self.with_temp("<tuple>", t, span, span, |lw, _| {
            lw.expr(&n, None, Want::Move)
        })
    }

    /// `recv.join(sep)` / `recv.toString()` when `recv` is a tuple; `Err` gives `recv` back for
    /// other receivers and methods.
    pub(super) fn tuple_text_call(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Tuple(elems) = self.cx.ty.kind(recv.ty).clone() else {
            return Err(recv);
        };
        let max = match prop.name.as_str() {
            "join" => 1,
            "toString" => 0,
            _ => return Err(recv),
        };
        if args.len() > max {
            return Err(recv);
        }
        let nullable: Vec<bool> = elems
            .iter()
            .map(|&e| self.cx.ty.opt_payload(e).is_some())
            .collect();
        let recv = self.share_if_place(recv);
        Ok(self.with_temp("<tuple>", recv, span, span, |lw, t| {
            let str_ = lw.cx.ty.str_;
            let sep = match args.first() {
                // `undefined` (a `string | undefined` separator) joins with `","`, as for an
                // array.
                Some(a) => {
                    let optional = lw
                        .trial_type(a)
                        .is_some_and(|t| lw.cx.ty.opt_payload(t).is_some());
                    let a = match optional {
                        true => binary(ast::BinaryOp::Nullish, a.clone(), str_lit(",", span), span),
                        false => a.clone(),
                    };
                    let h = lw.expr(&a, Some(str_), Want::Move);
                    lw.coerce(h, str_)
                }
                None => lw.str_lit(",", span),
            };
            lw.with_temp("<sep>", sep, span, span, |lw, s| {
                let mut text: Option<ast::Expr> = None;
                for (i, &nullable) in nullable.iter().enumerate() {
                    let e = element(t, i, span);
                    let part = if nullable {
                        let null = synth(ast::ExprKind::Lit(ast::Lit::Null), span);
                        let test = binary(ast::BinaryOp::Eq, e.clone(), null, span);
                        let kind = ast::ExprKind::Cond {
                            cond: Box::new(test),
                            then: Box::new(str_lit("", span)),
                            els: Box::new(string_of(e, span)),
                        };
                        synth(kind, span)
                    } else {
                        string_of(e, span)
                    };
                    text = Some(match text {
                        None => binary(ast::BinaryOp::Add, str_lit("", span), part, span),
                        Some(acc) => {
                            let acc = binary(ast::BinaryOp::Add, acc, s.clone(), span);
                            binary(ast::BinaryOp::Add, acc, part, span)
                        }
                    });
                }
                let text = text.unwrap_or_else(|| str_lit("", span));
                lw.expr(&text, Some(str_), Want::Move)
            })
        }))
    }
}

/// What [`FnCx::tuple_array_call`] calls on a tuple's elements read one at a time (with
/// `reduceFirst` and `reduceRightFirst`, the `reduce` and `reduceRight` of one argument).
const CALLING_BACK: &[&str] = &[
    "every",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flatMap",
    "forEach",
    "map",
    "reduce",
    "reduceRight",
    "some",
];

/// What [`FnCx::tuple_array_call`] applies to.
const ARRAY_READERS: &[&str] = &[
    "at",
    "concat",
    "entries",
    "every",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flat",
    "flatMap",
    "forEach",
    "includes",
    "indexOf",
    "keys",
    "lastIndexOf",
    "map",
    "reduce",
    "reduceRight",
    "slice",
    "some",
    "values",
];
