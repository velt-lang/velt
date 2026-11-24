//! `console.log("%s=%d", k, v)`: format specifiers in a literal format string, as Node's
//! `util.format` reads them (#3662, `crate::console_format`). The literal keeps its place as the
//! first argument, and each argument a specifier takes is converted here for it: `%d` to
//! `Number(x)`, `%i` to `parseInt(String(x))`, `%f` to `parseFloat(String(x))`, `%j` to
//! `JSON.stringify(x)`, `%s` of an object with a `toString()` of its own to its text, and `%s`
//! and `%d` of an `any` by std (`consoleString`, `consoleNumber`). As in Node, every argument is
//! evaluated before any is converted: when a conversion may run the program's code
//! (`toString()`, `valueOf()`, `toJSON()`), the arguments are evaluated into temporaries first.
//! velt_vir's console lowering splits the literal again and writes the pieces
//! (`lower/console_format.rs`). A format string that is not a literal is printed as it is,
//! followed by the other arguments: Node would read its specifiers.

use velt_common::Span;
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::FnCx;
use crate::collect::{lookup_method, Found};
use crate::console_format::{literal_text, split, Piece, BUILTIN_CLASSES};
use crate::hir::{self, ExprKind as H, Intrinsic, TyId, TyKind};

/// std's `%s` of an `any` (std/prelude/console_format.velt).
const STRING_FN: &str = "consoleString";
/// std's `%d` of an `any`: `Number(x)`, `NaN` for a symbol.
const NUMBER_FN: &str = "consoleNumber";
/// std's `JSON.stringify` by its path (std/prelude/json/json.velt), loaded for a `%j`
/// (velt_loader `globals/console_json.rs`).
const JSON_FN: &str = "std/prelude/json/json::jsonText";

/// A line's arguments after the format string: each one's specifier (`None`: left over) and
/// expression.
struct Args<'a> {
    specs: Vec<Option<u8>>,
    asts: &'a [ast::Expr],
}

impl FnCx<'_, '_> {
    /// `console.log(first, ...more)` with `first` checked as `h`: the whole call when `first` is
    /// a string literal whose specifiers format the line, else `first` as printed
    /// (`print_value`).
    pub(super) fn console_format(
        &mut self,
        intr: Intrinsic,
        first: &ast::Expr,
        h: hir::Expr,
        more: &[ast::Expr],
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        // What the lowering sees (console.rs asks `literal_text` of the same expression).
        let p = self.print_value(first, h);
        let Some(fmt) = literal_text(&p).map(str::to_string) else {
            return Err(p);
        };
        let Some(parts) = split(&fmt, more.len()) else {
            return Err(p);
        };
        let specs: Vec<Option<u8>> = (1..=more.len())
            .map(|k| {
                parts.pieces.iter().find_map(|p| match *p {
                    Piece::Arg(c, i) if i == k => Some(c),
                    _ => None,
                })
            })
            .collect();
        let values: Vec<hir::Expr> = more.iter().map(|a| self.printed_expr(a)).collect();
        let args = Args { specs, asts: more };
        // A conversion that may run the program's code waits until every argument is evaluated.
        // Not when an argument holds an `Atomic`: an atomic is used where it is, so it cannot be
        // moved into a temporary (velt_sema `atomic_copies.rs` reports the share that would
        // take), and such a line is converted in place, argument by argument.
        let ordered = values
            .iter()
            .zip(&args.specs)
            .any(|(v, s)| self.format_converts(*s, v.ty))
            && !values.iter().any(|v| self.cx.holds_atomic(v.ty));
        let line = match ordered {
            false => {
                let out = values.into_iter().enumerate();
                let out = out.map(|(k, v)| self.format_arg(&args, k, v)).collect();
                self.format_line(intr, p, out, span)
            }
            true => {
                let mut held = vec![];
                self.format_held(intr, p, &args, values, &mut held, span)
            }
        };
        Ok(line)
    }

    /// `Print(fmt, out…)`.
    fn format_line(
        &mut self,
        intr: Intrinsic,
        fmt: hir::Expr,
        mut out: Vec<hir::Expr>,
        span: Span,
    ) -> hir::Expr {
        out.insert(0, fmt);
        self.intrinsic(intr, out, self.cx.ty.unit, span)
    }

    /// The line with the arguments evaluated into temporaries one by one, in order (a literal
    /// is its own value), then converted from them.
    fn format_held(
        &mut self,
        intr: Intrinsic,
        fmt: hir::Expr,
        args: &Args<'_>,
        mut values: Vec<hir::Expr>,
        held: &mut Vec<HeldArg>,
        span: Span,
    ) -> hir::Expr {
        if values.is_empty() {
            let mut out = vec![];
            for (k, h) in held.iter_mut().enumerate() {
                let v = match std::mem::replace(h, HeldArg::Taken) {
                    HeldArg::Value(v) => v,
                    HeldArg::Temp(e) => self.printed_expr(&e),
                    HeldArg::Taken => panic!("ICE: console format: an argument taken twice"),
                };
                out.push(self.format_arg(args, k, v));
            }
            return self.format_line(intr, fmt, out, span);
        }
        let v = values.remove(0);
        if matches!(v.kind, H::Lit(_)) {
            held.push(HeldArg::Value(v));
            let r = self.format_held(intr, fmt, args, values, held, span);
            held.pop();
            return r;
        }
        let v = self.share_if_place(v);
        let name = format!("#arg{}", held.len());
        self.with_temp(&name, v, span, span, |lw, ident| {
            held.push(HeldArg::Temp(Box::new(ident.clone())));
            let r = lw.format_held(intr, fmt, args, values, held, span);
            held.pop();
            r
        })
    }

    /// May converting a value of type `t` for specifier `spec` run the program's code (its
    /// `toString()`, `valueOf()`, `toJSON()`), so that it must wait for the arguments after it?
    fn format_converts(&mut self, spec: Option<u8>, t: TyId) -> bool {
        let t = self.cx.widened(t);
        let primitive = matches!(
            self.cx.ty.kind(t),
            TyKind::Int(_)
                | TyKind::Float(_)
                | TyKind::Bool
                | TyKind::Str
                | TyKind::Literal(_)
                | TyKind::Unit
                | TyKind::Never
                | TyKind::Symbol
        );
        matches!(spec, Some(b's' | b'd' | b'i' | b'f' | b'j')) && !primitive
    }

    /// Argument `k` (its value `h`), converted for its specifier.
    fn format_arg(&mut self, args: &Args<'_>, k: usize, h: hir::Expr) -> hir::Expr {
        let (spec, a) = (args.specs[k], &args.asts[k]);
        let span = a.span;
        let t = self.cx.widened(h.ty);
        let kind = self.cx.ty.kind(t).clone();
        let int = matches!(kind, TyKind::Int(_));
        let symbol = matches!(kind, TyKind::Symbol);
        let dynamic = matches!(
            self.cx.dynamic(t),
            Some(Dynamic::Any | Dynamic::Unknown | Dynamic::Object)
        );
        let h = match spec {
            Some(b'j') => return self.std_call(JSON_FN, h, span),
            Some(b's') => return self.format_string(a, h),
            // A `BigInt` as Node writes a bigint: its digits and `n`; `%f` its number.
            Some(b'd' | b'i') if self.is_bigint(t) => {
                let text = self.js_text(h, "a `%d` or `%i` argument");
                let n = self.str_lit("n", span);
                self.concat(text, n, span)
            }
            Some(b'f') if self.is_bigint(t) => {
                let text = self.js_text(h, "a `%f` argument");
                self.std_call("parseFloat", text, span)
            }
            // An integer is its own value for all three.
            Some(b'd' | b'i' | b'f') if int => h,
            // Node writes `NaN` for a symbol, where `Number(s)` would throw.
            Some(b'd' | b'i' | b'f') if symbol => self.nan_after(h, span),
            Some(b'd') if dynamic => {
                self.note_dyn_members();
                self.std_call(NUMBER_FN, h, span)
            }
            Some(b'd') => match self.cx.prelude_fn("Number") {
                Some(d) => self.number_of(d, h, span),
                None => h,
            },
            Some(c @ (b'i' | b'f')) => {
                let name = if c == b'i' { "parseInt" } else { "parseFloat" };
                let text = self.js_text(h, "a `%i` or `%f` argument");
                self.std_call(name, text, span)
            }
            _ => h,
        };
        self.print_value(a, h)
    }

    /// `%s` of `a` (its value `h`): a `toString()` of its own (Node's `hasBuiltInToString` is
    /// false) is called; an `any` (and an interface value, which prints as one) is written
    /// by std's `consoleString`; anything else stays, and the lowering writes it (a primitive as
    /// `String(x)`, a class instance held by a nullable or a union by its own `toString()`, any
    /// other object inspected at depth 0).
    fn format_string(&mut self, a: &ast::Expr, h: hir::Expr) -> hir::Expr {
        let span = h.span;
        let t = self.cx.widened(h.ty);
        if self.is_bigint(t) {
            let text = self.js_text(h, "a `%s` argument");
            let n = self.str_lit("n", span);
            return self.concat(text, n, span);
        }
        let h = match self.own_text(t) {
            true => self.own_to_string(h, "toString"),
            false => h,
        };
        let p = self.print_value(a, h);
        let dynamic = matches!(
            self.cx.dynamic(p.ty),
            Some(Dynamic::Any | Dynamic::Unknown | Dynamic::Object)
        );
        match (dynamic, self.any_ty()) {
            (true, Some(any)) => {
                let p = self.share_if_place(p);
                let p = self.coerce(p, any);
                // std calls the `toString()` of the class it holds through `any`, as `String(x)`
                // of an `any` does: the classes' methods are callable through `any` then.
                self.note_dyn_members();
                self.std_call(STRING_FN, p, span)
            }
            _ => p,
        }
    }

    /// Does a `t` class instance have a `toString()` of its own: one a class that is not a
    /// JavaScript built-in declares (Node's `hasBuiltInToString` is false)?
    fn own_text(&mut self, t: TyId) -> bool {
        let Some((d, args)) = self.adt_of(t) else {
            return false;
        };
        let owner = match lookup_method(self.cx, d, &args, "toString") {
            Some(Found::Class { owner, .. }) => owner,
            _ => return false,
        };
        !self
            .cx
            .adt(owner)
            .is_some_and(|a| BUILTIN_CLASSES.contains(&a.name.as_str()))
    }

    /// A call of std's function `f` (a prelude name or a `module::name` path) on the value `h`.
    fn std_call(&mut self, f: &str, h: hir::Expr, span: Span) -> hir::Expr {
        self.with_temp("#value", h, span, span, |lw, v| {
            lw.prelude_call(f, "`console.log`", &[], std::slice::from_ref(v), None, span)
        })
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
}

/// An argument of a line converted from temporaries ([`FnCx::format_held`]).
enum HeldArg {
    /// A literal: its own value.
    Value(hir::Expr),
    /// The temporary holding it.
    Temp(Box<ast::Expr>),
    /// Already converted.
    Taken,
}
