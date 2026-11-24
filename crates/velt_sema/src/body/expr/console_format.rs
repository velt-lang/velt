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
use crate::hir::{self, DefId, ExprKind as H, Intrinsic, TyId, TyKind};

/// std's `%s` of an `any` (std/prelude/console_format.velt).
const STRING_FN: &str = "consoleString";
/// std's `%d` of an `any`: `Number(x)`, `NaN` for a symbol.
const NUMBER_FN: &str = "consoleNumber";
/// std's `%i` and `%f` of a number: `parseInt(String(n))`, `parseFloat(String(n))`.
const INTEGER_FN: &str = "consoleInteger";
const FLOAT_FN: &str = "consoleFloat";
/// std's `JSON.stringify` by its path (std/prelude/json/json.velt), loaded for a `%j`
/// (velt_loader `globals/console_json.rs`).
const JSON_FN: &str = "std/prelude/json/json::jsonText";
/// std's `%j` of an `any`: `undefined` for what `JSON.stringify` gives it for.
const JSON_ANY_FN: &str = "std/prelude/json/json_any::jsonAny";

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
        // A `%j` of a value std's JSON cannot write yet (a `Set`, an interface value): the line
        // is printed as without specifiers, its format no literal for the lowering.
        let unwritable = values
            .iter()
            .zip(&specs)
            .any(|(v, s)| *s == Some(b'j') && self.json_plan(v) == JsonPlan::Unwritable);
        if unwritable {
            let ty = p.ty;
            let block = hir::Block {
                stmts: vec![],
                value: Some(Box::new(p)),
                span,
            };
            let mut out = vec![self.mk(H::Block(block), ty, span)];
            for (a, v) in more.iter().zip(values) {
                out.push(self.print_value(a, v));
            }
            return Ok(self.intrinsic(intr, out, self.cx.ty.unit, span));
        }
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
    /// `toString()`, `valueOf()`, `toJSON()`, `[Symbol.toPrimitive]`, a custom inspect), so that
    /// it must wait for the arguments after it?
    fn format_converts(&mut self, spec: Option<u8>, t: TyId) -> bool {
        let t = self.cx.widened(t);
        matches!(spec, Some(b's' | b'd' | b'i' | b'f' | b'j')) && self.may_run_code(t, &mut vec![])
    }

    /// May a conversion of a value of `t` (`String`, `Number`, `JSON.stringify`, inspect) call
    /// the program's code: a value held by an `any`, an interface, a type parameter or a
    /// function, or a class (or what it holds) with one of the methods those call?
    fn may_run_code(&mut self, t: TyId, seen: &mut Vec<TyId>) -> bool {
        if seen.contains(&t) {
            return false;
        }
        seen.push(t);
        let runs = match self.cx.ty.kind(t).clone() {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Str
            | TyKind::Literal(_)
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Symbol => false,
            TyKind::Option(e, _) | TyKind::Array(e) => self.may_run_code(e, seen),
            TyKind::Tuple(es) => es.into_iter().any(|e| self.may_run_code(e, seen)),
            TyKind::Adt(d, args) => self.adt_may_run_code(d, &args, seen),
            _ => true,
        };
        seen.pop();
        runs
    }

    /// [`Self::may_run_code`] of the class, struct, enum or object literal type `d<args>`.
    fn adt_may_run_code(&mut self, d: DefId, args: &[TyId], seen: &mut Vec<TyId>) -> bool {
        if self.cx.is_json_value(d) {
            return false;
        }
        if let Some(e) = self.cx.enum_info(d) {
            let payloads: Vec<TyId> = e.variants.iter().flat_map(|v| v.payload.clone()).collect();
            return payloads.into_iter().any(|p| {
                let p = self.cx.subst(p, args);
                self.may_run_code(p, seen)
            });
        }
        const HOOKS: [&str; 5] = [
            "toString",
            "valueOf",
            "toJSON",
            "[Symbol.toPrimitive]",
            crate::hooks::INSPECT,
        ];
        for name in HOOKS {
            if let Some(Found::Class { owner, .. }) = lookup_method(self.cx, d, args, name) {
                // A built-in's (`Date`'s `toJSON()`) runs no code of the program's.
                if !self.cx.scopes[self.cx.adt(owner).map_or(0, |a| a.module)].is_std {
                    return true;
                }
            }
        }
        let fields: Vec<TyId> = self
            .cx
            .adt(d)
            .map(|a| a.fields.iter().map(|f| f.ty).collect())
            .unwrap_or_default();
        fields.into_iter().chain(args.to_vec()).any(|ft| {
            let ft = self.cx.subst(ft, args);
            self.may_run_code(ft, seen)
        })
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
            Some(b'j') => return self.format_json(h, span),
            Some(b's') => return self.format_string(a, h),
            // Inspected by the lowering, a class's own `[INSPECT]()` too (a `Date` unquoted).
            Some(b'o' | b'O') => return self.print_value_with(a, h, false),
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
            // A number without parsing its text: `parseFloat(String(x))` is `x` (`0` for
            // `-0`), `parseInt(String(x))` std's `consoleInteger`.
            Some(c @ (b'i' | b'f')) if matches!(kind, TyKind::Float(_)) => {
                let f64 = self.cx.ty.f64;
                let h = self.coerce(h, f64);
                let name = if c == b'i' { INTEGER_FN } else { FLOAT_FN };
                self.std_call(name, h, span)
            }
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
        let own = self.own_text(t);
        let h = match own {
            // Before `print_value`, which would call its `[INSPECT]()` instead.
            OwnText::ToString => self.own_to_string(h, "toString"),
            _ => h,
        };
        let p = self.print_value(a, h);
        // A `[Symbol.toPrimitive]` is called by `String(x)` of the `any` (std's dispatch over
        // the program's classes, which also gives the hint).
        let dynamic = own == OwnText::ToPrimitive
            || matches!(
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

    /// Has a `t` class instance a text of its own (Node's `hasBuiltInToString` is false): a
    /// `[Symbol.toPrimitive]` or a `toString()` that a class other than a JavaScript built-in
    /// declares? A class with both its own `toString()` and an `[INSPECT]()` is the case only
    /// sema sees: the lowering gets the inspect hook's text.
    fn own_text(&mut self, t: TyId) -> OwnText {
        let Some((d, args)) = self.adt_of(t) else {
            return OwnText::None;
        };
        let own = |lw: &mut Self, name: &str| match lookup_method(lw.cx, d, &args, name) {
            Some(Found::Class { owner, .. }) => !lw
                .cx
                .adt(owner)
                .is_some_and(|a| BUILTIN_CLASSES.contains(&a.name.as_str())),
            _ => false,
        };
        if own(self, "[Symbol.toPrimitive]") {
            OwnText::ToPrimitive
        } else if own(self, "toString") {
            OwnText::ToString
        } else {
            OwnText::None
        }
    }

    /// How `%j` writes a value of `v`'s type.
    fn json_plan(&mut self, v: &hir::Expr) -> JsonPlan {
        let t = self.cx.widened(v.ty);
        if matches!(
            self.cx.ty.kind(t),
            TyKind::Unit | TyKind::Symbol | TyKind::FnPtr { .. } | TyKind::Closure(_)
        ) {
            return JsonPlan::Undefined;
        }
        if self.cx.dynamic(t).is_some() {
            return JsonPlan::Dynamic;
        }
        let mut seen = vec![];
        if self.json_writable(t, &mut seen) {
            return JsonPlan::Typed;
        }
        // Anything else (a nullable `number | undefined`, a `Set`, an interface value) as an
        // `any`, which std writes as Node does (`undefined`, `{}`, the fields); a value that
        // cannot be one prints its line without specifiers.
        let Some(any) = self.any_ty() else {
            return JsonPlan::Unwritable;
        };
        let probe = v.clone();
        let boxed = self.trial_with(|lw| {
            let h = lw.share_if_place(probe);
            lw.coerce(h, any).ty
        });
        match boxed {
            Some(_) => JsonPlan::Dynamic,
            None => JsonPlan::Unwritable,
        }
    }

    /// Can std's `JSON.stringify` write a value of `t` as Node does (std/encoding/json/generate/
    /// check.velt's rules, conservatively: numbers, booleans, strings, literal types, enums
    /// without payloads, unions of those, arrays, tuples, `T | null`, `JsonValue`, and classes,
    /// structs and object literals with a `toJSON()` or with public fields of those, none holding
    /// itself)? Anything else goes through `any` (`jsonAny`). `seen`: the types being checked.
    fn json_writable(&mut self, t: TyId, seen: &mut Vec<TyId>) -> bool {
        // A type that can hold itself may hold a cycle: std's `jsonAny` finds it (`[Circular]`).
        if seen.contains(&t) {
            return false;
        }
        seen.push(t);
        let ok = match self.cx.ty.kind(t).clone() {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str | TyKind::Literal(_) => {
                true
            }
            TyKind::Option(e, n) => !n.has_undefined() && self.json_writable(e, seen),
            TyKind::Array(e) => self.json_writable(e, seen),
            TyKind::Tuple(es) => es.into_iter().all(|e| self.json_writable(e, seen)),
            TyKind::Adt(d, _) if self.cx.is_json_value(d) => true,
            // A `Map` is `{}` in Node; std's JSON writes its entries (`jsonAny` writes `{}`).
            TyKind::Adt(d, _) if self.cx.prelude_adt("Map") == Some(d) => false,
            TyKind::Adt(d, args) if self.cx.enum_info(d).is_some() => {
                self.json_enum_writable(d, &args, seen)
            }
            TyKind::Adt(d, args) => self.json_fields_writable(d, &args, seen),
            // A type parameter's value, an interface value, a `Set` field: through `any`.
            _ => false,
        };
        seen.pop();
        ok
    }

    /// [`Self::json_writable`] of an enum or a union type `d<args>`: an enum without payloads
    /// (its numbers or strings), a union whose every member is writable.
    fn json_enum_writable(&mut self, d: DefId, args: &[TyId], seen: &mut Vec<TyId>) -> bool {
        let Some(e) = self.cx.enum_info(d) else {
            return false;
        };
        let union = e.is_union;
        let payloads: Vec<TyId> = e.variants.iter().flat_map(|v| v.payload.clone()).collect();
        if !union {
            return payloads.is_empty();
        }
        payloads.into_iter().all(|p| {
            let p = self.cx.subst(p, args);
            self.json_writable(p, seen)
        })
    }

    /// [`Self::json_writable`] of a class, struct or object literal type `d<args>`.
    fn json_fields_writable(&mut self, d: DefId, args: &[TyId], seen: &mut Vec<TyId>) -> bool {
        if crate::hooks::hook(self.cx, d, args, "toJSON").is_some() {
            return true;
        }
        let Some(a) = self.cx.adt(d) else {
            return false;
        };
        let fields: Vec<(TyId, bool)> = a
            .fields
            .iter()
            .map(|f| (f.ty, f.private_to.is_some()))
            .collect();
        fields.into_iter().all(|(ft, private)| {
            let ft = self.cx.subst(ft, args);
            !private && self.json_writable(ft, seen)
        })
    }

    /// `%j` of `h`: `JSON.stringify(h)`, `undefined` for a value it gives `undefined` for (an
    /// `undefined`, a symbol, a function), an `any` (or a nullable) by std's `jsonAny`.
    fn format_json(&mut self, h: hir::Expr, span: Span) -> hir::Expr {
        match self.json_plan(&h) {
            JsonPlan::Undefined => {
                let text = self.str_lit("undefined", span);
                self.text_after(h, text, span)
            }
            JsonPlan::Dynamic => match self.any_ty() {
                Some(any) => {
                    let h = self.share_if_place(h);
                    let h = self.coerce(h, any);
                    self.note_dyn_members();
                    self.std_call(JSON_ANY_FN, h, span)
                }
                None => self.std_call(JSON_FN, h, span),
            },
            // `Unwritable` lines are printed without specifiers (`console_format`).
            JsonPlan::Typed | JsonPlan::Unwritable => self.std_call(JSON_FN, h, span),
        }
    }

    /// A call of std's function `f` (a prelude name or a `module::name` path) on the value `h`.
    /// A direct call (no temporary): the other parameters take their defaults, a type parameter
    /// the value's type.
    fn std_call(&mut self, f: &str, h: hir::Expr, span: Span) -> hir::Expr {
        if let Some(d) = self.cx.prelude_fn(f) {
            crate::body::param_defaults(self.cx, d);
            let info = self.cx.fn_info(d);
            let generic = !info.generics.names.is_empty();
            let ret = info.ret;
            let rest: Option<Vec<hir::Expr>> = info
                .params
                .iter()
                .skip(1)
                .map(|p| p.default.clone())
                .collect();
            if let (Some(rest), 1..) = (rest, info.params.len()) {
                let targs = if generic { vec![h.ty] } else { vec![] };
                let ret = self.cx.subst(ret, &targs);
                let mut args = vec![h];
                args.extend(rest);
                let kind = H::Call {
                    callee: hir::Callee::Def(d, targs),
                    args,
                };
                return self.mk(kind, ret, span);
            }
        }
        self.with_temp("#value", h, span, span, |lw, v| {
            lw.prelude_call(f, "`console.log`", &[], std::slice::from_ref(v), None, span)
        })
    }

    /// `NaN`, after evaluating `h` for its effects.
    fn nan_after(&mut self, h: hir::Expr, span: Span) -> hir::Expr {
        let f64 = self.cx.ty.f64;
        let nan = self.mk(H::Lit(hir::Lit::Float(f64::NAN)), f64, span);
        self.text_after(h, nan, span)
    }

    /// `value`, after evaluating `h` for its effects.
    fn text_after(&mut self, h: hir::Expr, value: hir::Expr, span: Span) -> hir::Expr {
        let ty = value.ty;
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(h),
                span,
            }],
            value: Some(Box::new(value)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }
}

/// Which text of its own a class instance has for `%s` ([`FnCx::own_text`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum OwnText {
    None,
    ToString,
    ToPrimitive,
}

/// How `%j` writes a value ([`FnCx::json_plan`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum JsonPlan {
    /// By `JSON.stringify` of its type.
    Typed,
    /// `undefined`, as `JSON.stringify` gives for it (an `undefined`, a symbol, a function).
    Undefined,
    /// As an `any`, by std's `jsonAny`.
    Dynamic,
    /// Not yet: the line is printed without specifiers.
    Unwritable,
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
