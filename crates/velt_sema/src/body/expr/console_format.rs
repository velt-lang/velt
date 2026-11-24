//! `console.log("%s=%d", k, v)`: format specifiers as Node's `util.format` reads them (#3662,
//! `crate::console_format`). A format string known when compiling (a string literal) keeps
//! its place as the first argument, and each argument a specifier takes is converted here for
//! it: `%d` to `Number(x)`, `%i` to `parseInt(String(x))`, `%f` to `parseFloat(String(x))`, `%j`
//! to `JSON.stringify(x)`, and `%s` of an object with a `toString()` of its own to its text.
//! velt_vir's console lowering splits the literal again and writes the pieces
//! (`lower/console_format.rs`). Any other string is tested for a `%` when the program runs and
//! then formatted by std (std/prelude/console_format.velt), one argument at a time.

use velt_common::Span;
use velt_syntax::ast;

use super::callback::synth;
use crate::any::Dynamic;
use crate::body::{FnCx, Want};
use crate::collect::{lookup_method, Found};
use crate::console_format::{literal_text, split, Piece, BUILTIN_CLASSES};
use crate::hir::{self, ExprKind as H, Intrinsic, TyId, TyKind};

/// std's `%s` of an `any`.
const STRING_FN: &str = "consoleString";
/// std's test for a `%` in a format string known only when the program runs.
const PERCENT_FN: &str = "consoleHasPercent";
/// std's line being formatted, and its text.
const LINE_FN: &str = "consoleLine";
const LINE_END_FN: &str = "consoleLineEnd";
/// std's writer of an `any` argument with `%j` by `JSON.stringify`, in a program that loads
/// `JSON` (std/prelude/global/console_json.velt).
const ARG_ANY_JSON_FN: &str = "std/lang/hooks/console_json::consoleArgAnyJson";

impl FnCx<'_, '_> {
    /// `console.log(first, ...more)` with `first` checked as `h`: the whole call when `first` is a
    /// string whose specifiers format the line, else `first` as printed (`print_value`).
    pub(super) fn console_format(
        &mut self,
        intr: Intrinsic,
        first: &ast::Expr,
        h: hir::Expr,
        more: &[ast::Expr],
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let kind = self.cx.ty.kind(h.ty).clone();
        // The text of one value (`String(x)`, `${x}`) is printed as it is: a label, which would
        // otherwise take std's run-time formatter, and `any` with it, into the program.
        let value_text = matches!(
            &h.kind,
            H::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::ToString | Intrinsic::TemplateToString),
                ..
            }
        );
        if kind == TyKind::Str && !value_text && literal_text(&h, &kind).is_none() {
            return Ok(self.format_at_run_time(intr, first, h, more, span));
        }
        // What the lowering sees (console.rs asks `literal_text` of the same expression).
        let p = self.print_value(first, h);
        let kind = self.cx.ty.kind(p.ty).clone();
        let Some(fmt) = literal_text(&p, &kind) else {
            return Err(p);
        };
        let Some(parts) = split(&fmt, more.len()) else {
            return Err(p);
        };
        let mut out = vec![p];
        for (k, a) in more.iter().enumerate() {
            let spec = parts.pieces.iter().find_map(|p| match *p {
                Piece::Arg(c, i) if i == k + 1 => Some(c),
                _ => None,
            });
            let v = self.format_arg(spec, a);
            out.push(v);
        }
        Ok(self.intrinsic(intr, out, self.cx.ty.unit, span))
    }

    /// Argument `a`, which specifier `spec` takes (`None`: a leftover one), converted for it.
    fn format_arg(&mut self, spec: Option<u8>, a: &ast::Expr) -> hir::Expr {
        let span = a.span;
        if spec == Some(b'j') {
            return self.format_json(a);
        }
        let h = self.printed_expr(a);
        let t = self.cx.widened(h.ty);
        let kind = self.cx.ty.kind(t).clone();
        let int = matches!(kind, TyKind::Int(_));
        let symbol = matches!(kind, TyKind::Symbol);
        let h = match spec {
            Some(b's') => self.format_string(h),
            // A bigint is written as itself (`123n`), an integer is its own value for all three.
            Some(b'd' | b'i' | b'f') if int || self.is_bigint(t) => h,
            // Node writes `NaN` for a symbol, where `Number(s)` would throw.
            Some(b'd' | b'i' | b'f') if symbol => self.nan_after(h, span),
            Some(b'd') => match self.cx.prelude_fn("Number") {
                Some(d) => self.number_of(d, h, span),
                None => h,
            },
            Some(c @ (b'i' | b'f')) => {
                let name = if c == b'i' { "parseInt" } else { "parseFloat" };
                let text = self.js_text(h, "a `%i` or `%f` argument");
                self.with_temp("#text", text, span, span, |lw, v| {
                    lw.prelude_call(name, name, &[], std::slice::from_ref(v), None, span)
                })
            }
            _ => h,
        };
        self.print_value(a, h)
    }

    /// `%s` of `h`: a `toString()` of its own (Node's `hasBuiltInToString` is false) is called;
    /// an `any` is written by std's `consoleString`; anything else stays, and the lowering
    /// writes it (a primitive as `String(x)`, an object inspected at depth 0).
    fn format_string(&mut self, h: hir::Expr) -> hir::Expr {
        let span = h.span;
        let t = self.cx.widened(h.ty);
        if matches!(
            self.cx.dynamic(t),
            Some(Dynamic::Any | Dynamic::Unknown | Dynamic::Object)
        ) {
            if let Some(any) = self.any_ty() {
                let h = self.share_if_place(h);
                let h = self.coerce(h, any);
                return self.with_temp("#value", h, span, span, |lw, v| {
                    let args = std::slice::from_ref(v);
                    lw.prelude_call(STRING_FN, "`%s`", &[], args, None, span)
                });
            }
            return h;
        }
        if self.is_bigint(t) {
            return h;
        }
        let Some((d, args)) = self.adt_of(t) else {
            return h;
        };
        let owner = match lookup_method(self.cx, d, &args, "toString") {
            Some(Found::Class { owner, .. }) => owner,
            _ => return h,
        };
        let builtin = self
            .cx
            .adt(owner)
            .is_some_and(|a| BUILTIN_CLASSES.contains(&a.name.as_str()));
        if builtin {
            return h;
        }
        self.own_to_string(h, "toString")
    }

    /// `%j` of `a`: `JSON.stringify(a)`.
    fn format_json(&mut self, a: &ast::Expr) -> hir::Expr {
        let span = a.span;
        let ident = |name: &str| ast::Ident {
            name: name.into(),
            span,
        };
        let json = synth(ast::ExprKind::Ident(ident("JSON")), span);
        let callee = synth(
            ast::ExprKind::Member {
                object: Box::new(json),
                prop: ident("stringify"),
                optional: false,
            },
            span,
        );
        let call = synth(
            ast::ExprKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args: vec![a.clone()],
                optional: false,
            },
            span,
        );
        let h = self.expr(&call, None, Want::Move);
        self.print_value(a, h)
    }

    /// `NaN`, after evaluating `h` for its effects.
    fn nan_after(&mut self, h: hir::Expr, span: Span) -> hir::Expr {
        let f64 = self.cx.ty.f64;
        let nan = self.mk(H::Lit(hir::Lit::Float(f64::NAN)), f64, span);
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(h),
                span,
            }],
            value: Some(Box::new(nan)),
            span,
        };
        self.mk(H::Block(block), f64, span)
    }

    /// `console.log(fmt, ...more)` with a format string known only when the program runs: a
    /// format string holding a `%` is written by std one argument at a time, and any other is
    /// printed as without specifiers. Every argument is evaluated once: a variable or a path of
    /// fields is read where it is in either branch, any other value is evaluated into a
    /// temporary first.
    fn format_at_run_time(
        &mut self,
        intr: Intrinsic,
        first: &ast::Expr,
        h: hir::Expr,
        more: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let mut values = vec![h];
        for a in more {
            values.push(self.printed_expr(a));
        }
        let asts: Vec<&ast::Expr> = std::iter::once(first).chain(more).collect();
        // An `Atomic` is used where it is: it cannot be held by a temporary or an `any`, so a
        // line printing one is printed as without specifiers.
        if values.iter().any(|v| self.cx.holds_atomic(v.ty)) {
            let printed = asts.iter().zip(values);
            let out = printed.map(|(a, h)| self.print_value(a, h)).collect();
            return self.intrinsic(intr, out, self.cx.ty.unit, span);
        }
        let writers: Vec<&'static str> =
            values[1..].iter().map(|v| self.arg_writer(v.ty)).collect();
        let mut args = vec![];
        for (a, h) in asts.into_iter().zip(values) {
            args.push(match is_path(a) && super::accessor_rmw::is_field_path(&h) {
                true => Arg::Path(Box::new(a.clone()), h),
                false => Arg::Temp(self.share_if_place(h)),
            });
        }
        let mut done = vec![];
        let line = Line {
            intr,
            first,
            more,
            writers: &writers,
            span,
        };
        self.format_temps(&line, args, &mut done)
    }

    /// std's writer of a format argument of type `t`: a number, a string and a boolean by their
    /// own, any other value as an `any`.
    fn arg_writer(&mut self, t: TyId) -> &'static str {
        let t = self.cx.widened(t);
        match self.cx.ty.kind(t) {
            TyKind::Float(_) => "consoleArgNumber",
            TyKind::Int(it) if it.is_signed() => "consoleArgInt",
            TyKind::Int(_) => "consoleArgUint",
            TyKind::Str => "consoleArgString",
            TyKind::Bool => "consoleArgBool",
            _ if self.cx.std_def(ARG_ANY_JSON_FN).is_some() => ARG_ANY_JSON_FN,
            _ => "consoleArgAny",
        }
    }

    /// Binds the arguments that need one to temporaries, one by one (`done` holds each
    /// argument's expression for the formatter and its value for the plain line), then writes
    /// the line.
    fn format_temps(
        &mut self,
        line: &Line<'_>,
        mut args: Vec<Arg>,
        done: &mut Vec<(ast::Expr, Option<hir::Expr>)>,
    ) -> hir::Expr {
        if args.is_empty() {
            return self.format_choice(line, done);
        }
        match args.remove(0) {
            Arg::Path(a, h) => {
                let a = *a;
                done.push((a, Some(h)));
                let r = self.format_temps(line, args, done);
                done.pop();
                r
            }
            Arg::Temp(v) => {
                let (name, span) = (format!("#fmt{}", done.len()), line.span);
                self.with_temp(&name, v, span, span, |lw, ident| {
                    done.push((ident.clone(), None));
                    let r = lw.format_temps(line, args, done);
                    done.pop();
                    r
                })
            }
        }
    }

    /// `if (consoleHasPercent(fmt)) <formatted> else <plain>` over the arguments `done`.
    fn format_choice(
        &mut self,
        line: &Line<'_>,
        done: &mut [(ast::Expr, Option<hir::Expr>)],
    ) -> hir::Expr {
        let (unit, span) = (self.cx.ty.unit, line.span);
        let mut plain = vec![];
        let asts = std::iter::once(line.first).chain(line.more);
        for ((e, h), a) in done.iter_mut().zip(asts) {
            let h = match h.take() {
                Some(h) => h,
                None => self.printed_expr(e),
            };
            plain.push(self.print_value(a, h));
        }
        let els = self.intrinsic(line.intr, plain, unit, span);
        let exprs: Vec<ast::Expr> = done.iter().map(|(e, _)| e.clone()).collect();
        // An argument that cannot be an `any` (a pointer, a function of some types): the line
        // is printed as without specifiers.
        if self
            .trial_with(|lw| lw.formatted(line, &exprs).ty)
            .is_none()
        {
            return els;
        }
        let what = "`console.log`";
        let cond = self.prelude_call(PERCENT_FN, what, &[], &exprs[..1], None, span);
        let text = self.formatted(line, &exprs);
        let then = self.intrinsic(line.intr, vec![text], unit, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        };
        self.mk(kind, unit, span)
    }

    /// `{ const l = consoleLine(fmt); consoleArg…(l, a); …; consoleLineEnd(l) }`: the line of
    /// the arguments `exprs` (the format string first), as std formats it.
    fn formatted(&mut self, line: &Line<'_>, exprs: &[ast::Expr]) -> hir::Expr {
        let (span, what) = (line.span, "`console.log`");
        let start = self.prelude_call(LINE_FN, what, &[], &exprs[..1], None, span);
        self.with_temp("#line", start, span, span, |lw, l| {
            let mut stmts = vec![];
            for (e, f) in exprs[1..].iter().zip(line.writers) {
                let call = lw.prelude_call(f, what, &[], &[l.clone(), e.clone()], None, span);
                stmts.push(hir::Stmt {
                    kind: hir::StmtKind::Expr(call),
                    span,
                });
            }
            let end = lw.prelude_call(LINE_END_FN, what, &[], std::slice::from_ref(l), None, span);
            let ty = end.ty;
            let block = hir::Block {
                stmts,
                value: Some(Box::new(end)),
                span,
            };
            lw.mk(H::Block(block), ty, span)
        })
    }
}

/// A line formatted when the program runs: the call, its arguments, and std's writer of each
/// argument after the format string.
struct Line<'a> {
    intr: Intrinsic,
    first: &'a ast::Expr,
    more: &'a [ast::Expr],
    writers: &'a [&'static str],
    span: Span,
}

/// An argument of a line formatted when the program runs.
enum Arg {
    /// A variable or a path of fields (its expression, checked again in the formatted branch,
    /// and its value for the plain line).
    Path(Box<ast::Expr>, hir::Expr),
    /// Any other value, evaluated into a temporary first.
    Temp(hir::Expr),
}

/// Is `e` a variable or a path of fields of one, which reading again changes nothing?
fn is_path(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Paren(x) => is_path(x),
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_path(object),
        _ => false,
    }
}
