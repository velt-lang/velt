//! Literals (incl. `null`), string concatenation and template literals.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::any_text::Hint;
use crate::any::Dynamic;
use crate::body::{FnCx, Want};
use crate::defs::{BodyState, DefInfo};
use crate::hir::{self, Callee, ExprKind as H, Intrinsic, Nothing, TyId, TyKind};
use crate::types::{int_max, int_name, int_range};

impl FnCx<'_, '_> {
    pub(crate) fn lit(
        &mut self,
        l: &ast::Lit,
        exp: Option<TyId>,
        span: Span,
        negated: bool,
    ) -> hir::Expr {
        let exp = match l {
            ast::Lit::Null => return self.null_lit(exp, span),
            ast::Lit::Undefined => return self.undefined_lit(exp, span),
            _ => self.hint(exp),
        };
        // A literal expected as a union takes the type of the member it belongs to.
        let exp = exp.map(|t| match self.cx.union_def(t) {
            Some(_) => self
                .lit_member_quiet(
                    t,
                    ast::SignedLit {
                        lit: l.clone(),
                        negative: false,
                    },
                )
                .map_or(t, |(_, m)| m),
            None => t,
        });
        match l {
            ast::Lit::Int { value, suffix } => {
                self.int_lit(*value, suffix.as_deref(), exp, span, negated)
            }
            ast::Lit::Float { value, suffix } => {
                self.float_lit(*value, suffix.as_deref(), exp, span)
            }
            ast::Lit::Str(s) => self.str_lit(s, span),
            ast::Lit::Bool(b) => self.mk(H::Lit(hir::Lit::Bool(*b)), self.cx.ty.bool_, span),
            ast::Lit::Null | ast::Lit::Undefined => unreachable!("ICE: nothings handled above"),
        }
    }

    /// Type named by a literal suffix (`5u8`, `1.0f32`), if valid for this kind of literal.
    fn suffix_ty(&mut self, s: &str, float_only: bool, span: Span) -> Option<TyId> {
        let t = self.cx.ty.primitive(s).filter(|t| {
            let ty = &self.cx.ty;
            if float_only {
                ty.is_float(*t)
            } else {
                ty.is_numeric(*t)
            }
        });
        if t.is_none() {
            let what = if float_only { "float" } else { "number" };
            self.cx
                .err(format!("invalid suffix `{s}` for {what} literal"), span);
        }
        t
    }

    fn int_lit(
        &mut self,
        value: u128,
        suffix: Option<&str>,
        exp: Option<TyId>,
        span: Span,
        negated: bool,
    ) -> hir::Expr {
        // Without a suffix or an integer type expected, a literal is a number (`literal_locals`
        // may make a local declared from it an integer).
        let ty = match suffix {
            Some(s) => match self.suffix_ty(s, false, span) {
                Some(t) => t,
                None => return self.error_expr(span),
            },
            None => exp
                .filter(|e| self.cx.ty.is_numeric(*e))
                .unwrap_or(self.cx.ty.f64),
        };
        if suffix.is_none() && ty == self.cx.ty.f64 {
            self.literal_number_lit(span);
        }
        if self.cx.ty.is_float(ty) {
            return self.mk(H::Lit(hir::Lit::Float(value as f64)), ty, span);
        }
        let it = self.cx.ty.int_ty(ty).expect("ICE: int literal type");
        if value > int_max(it, negated) {
            let shown = if negated {
                format!("-{value}")
            } else {
                value.to_string()
            };
            self.cx.error(
                Diagnostic::error(format!("literal out of range for `{}`", int_name(it)), span)
                    .with_note(format!(
                        "the literal `{shown}` does not fit into the type `{}` whose range is `{}`",
                        int_name(it),
                        int_range(it)
                    )),
            );
        }
        self.mk(H::Lit(hir::Lit::Int(value)), ty, span)
    }

    fn float_lit(
        &mut self,
        value: f64,
        suffix: Option<&str>,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let ty = match suffix {
            Some(s) => match self.suffix_ty(s, true, span) {
                Some(t) => t,
                None => return self.error_expr(span),
            },
            None => exp
                .filter(|e| self.cx.ty.is_float(*e))
                .unwrap_or(self.cx.ty.f64),
        };
        self.mk(H::Lit(hir::Lit::Float(value)), ty, span)
    }

    /// The value of field `name` of an object literal of its own type. Converted to an `any`
    /// or `unknown` (`dynamic`: `const u: unknown = { a: null }`), a `null` field (TypeScript's
    /// type `null`) is an `any` holding `null`, and so is one in a nested object or array
    /// literal (`{ b: { c: null } }`, `{ a: [null, 1] }`). A conditional, `&&`, `||` or `??`
    /// with an `any` value is an `any` there too (`{ q: c ? 1 : a.q }`, `any_join`).
    pub(super) fn anon_prop_value(
        &mut self,
        name: &ast::Ident,
        value: Option<&ast::Expr>,
        dynamic: bool,
    ) -> hir::Expr {
        let any = match value.is_some_and(|v| dynamic && dynamic_hinted(v)) {
            true => self.any_ty(),
            false => None,
        };
        self.prop_value(name, value, any)
    }

    /// The element type of an array literal of `elems` where a `t` is expected: `t`'s, or for an
    /// `any` (or `unknown`) `t` when an element is a conditional, `&&`, `||` or `??` (`[c ? 1 : a.q]`), or
    /// a literal holding or spreading one, whose `any` value stays one (`any_join`).
    pub(super) fn literal_elem(&mut self, t: TyId, elems: &[ast::Expr]) -> Option<TyId> {
        let elem = self.cx.ty.array_elem(t).or(self.cx.iterable_elem(t));
        elem.or_else(|| (self.cx.dynamic(t).is_some() && elems.iter().any(holds_join)).then_some(t))
    }

    /// `null` takes its `T | null` type from context.
    fn null_lit(&mut self, exp: Option<TyId>, span: Span) -> hir::Expr {
        if let Some(h) = exp.and_then(|t| self.nothing_in_member(Nothing::Null, t, span)) {
            return h;
        }
        match exp {
            Some(t) if self.cx.dynamic(t).is_some() => self.dynamic_nothing(Nothing::Null, t, span),
            Some(t) if self.cx.ty.nothing_of(t) == Some(Nothing::Undefined) => {
                self.nothing_mismatch("null", t, span)
            }
            Some(t) if self.is_three_state(t) && !self.cx.ty.has_error(t) => {
                self.nothing_value(Nothing::Null, t, span)
            }
            Some(t)
                if self
                    .cx
                    .ty
                    .opt_payload(t)
                    .is_some_and(|p| !self.cx.ty.has_error(p)) =>
            {
                self.mk(H::Lit(hir::Lit::Null), t, span)
            }
            // A type argument still to infer (`tag(null)`): `null` alone.
            Some(t) if t == self.cx.ty.error => {
                let t = self.cx.ty.null_alone();
                self.mk(H::Lit(hir::Lit::Null), t, span)
            }
            Some(t) if !self.cx.ty.has_error(t) => {
                let tn = self.cx.display(t);
                self.cx.error(
                    Diagnostic::error("mismatched types", span)
                        .with_note(format!("expected {tn}, found null"))
                        .with_note(format!("make the type nullable: `{tn} | null`")),
                );
                self.error_expr(span)
            }
            _ => {
                self.cx.error(
                    Diagnostic::error("cannot infer the type of `null`", span)
                        .with_note("add a type annotation like `let x: T | null = null`"),
                );
                self.error_expr(span)
            }
        }
    }

    /// `undefined` takes its `T | undefined` type from context (issue #930). In HIR it is the
    /// nothing of that option type, `Lit::Null` (hir_encodings.md "Undefined").
    fn undefined_lit(&mut self, exp: Option<TyId>, span: Span) -> hir::Expr {
        self.cx.writes_undefined |= !self.cx.scopes[self.module].is_std;
        match exp {
            Some(t) if self.cx.dynamic(t).is_some() => {
                self.dynamic_nothing(Nothing::Undefined, t, span)
            }
            // A type argument still to infer (`tag(undefined)`): `undefined` alone, as below.
            Some(t) if t == self.cx.ty.error => {
                let t = self.cx.ty.undefined();
                self.mk(H::Lit(hir::Lit::Null), t, span)
            }
            Some(t) if self.cx.ty.has_error(t) => self.error_expr(span),
            Some(t) if self.cx.ty.nothing_of(t).is_some_and(|n| n.has_undefined()) => {
                self.mk(H::Lit(hir::Lit::Null), t, span)
            }
            Some(t) => match self.nothing_in_member(Nothing::Undefined, t, span) {
                Some(h) => h,
                None => self.nothing_mismatch("undefined", t, span),
            },
            // With no type to take, its type is `undefined` alone.
            None => {
                let t = self.cx.ty.undefined();
                self.mk(H::Lit(hir::Lit::Null), t, span)
            }
        }
    }

    /// `void operand` as a value: evaluates `operand`, then gives `undefined` (typed from
    /// context, like the `undefined` literal). As a statement it is `StmtKind::Expr` of the
    /// operand (`expr_stmt`).
    pub(crate) fn void_expr(
        &mut self,
        operand: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let value = self.expr(operand, None, Want::Borrow);
        let undefined = self.undefined_lit(exp, span);
        if undefined.ty == self.cx.ty.error {
            return undefined;
        }
        let ty = undefined.ty;
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(value),
                span: operand.span,
            }],
            value: Some(Box::new(undefined)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    /// `e` is given for an optional parameter or field (`slot`): records its span, and those
    /// of the branches of a conditional (`c ? v : undefined`), for `optional_slot_error`.
    pub(crate) fn optional_slot(&mut self, e: &ast::Expr, slot: crate::body::OptionalSlot) {
        self.optional_slots.push((e.span, slot));
        match &e.kind {
            ast::ExprKind::Paren(inner) => self.optional_slot(inner, slot),
            ast::ExprKind::Cond { then, els, .. } => {
                self.optional_slot(then, slot);
                self.optional_slot(els, slot);
            }
            _ => {}
        }
    }

    /// An `undefined` (a literal, or a value that may be one) at `span`, given for a parameter
    /// with a default whose type does not allow `undefined`: JavaScript applies the default,
    /// which calls do not do yet (`true`: reported, issue #930). An optional parameter or field
    /// is a `T | undefined` and takes it as is. `maybe`: the value only may be `undefined`.
    pub(crate) fn optional_slot_error(&mut self, span: Span, maybe: bool) -> bool {
        let Some(&(_, slot)) = self.optional_slots.iter().rev().find(|(s, _)| *s == span) else {
            return false;
        };
        let (what, note) = match slot {
            crate::body::OptionalSlot::Param if maybe => (
                "a value that may be `undefined` passed for a parameter with a default",
                "JavaScript applies the default where it is `undefined`; test it first, or pass `value ?? default`",
            ),
            crate::body::OptionalSlot::Param => (
                "`undefined` passed for a parameter with a default",
                "JavaScript treats it as a left-out argument (the default applies); leave the argument out instead",
            ),
            crate::body::OptionalSlot::Field => (
                "`undefined` as the value of an optional field",
                "JavaScript keeps a field holding `undefined` apart from a field left out; leave the field out instead",
            ),
        };
        self.cx.error(
            Diagnostic::error(format!("{what} is not supported yet (#930)"), span).with_note(note),
        );
        true
    }

    /// `null` / `undefined` (`what`) where `t` does not allow it (TS2322).
    fn nothing_mismatch(&mut self, what: &str, t: TyId, span: Span) -> hir::Expr {
        let param = |s: &Self| {
            s.optional_slots
                .iter()
                .rev()
                .find(|(sp, _)| *sp == span)
                .is_some_and(|(_, k)| *k == crate::body::OptionalSlot::Param)
        };
        if what == "undefined" && param(self) {
            // For a parameter with a default, which the call gives instead (`default_args.rs`).
            let u = self.cx.ty.undefined();
            return self.mk(H::Lit(hir::Lit::Null), u, span);
        }
        if what == "undefined" && self.optional_slot_error(span, false) {
            return self.error_expr(span);
        }
        if what == "undefined" && t == self.cx.ty.unit {
            // TypeScript accepts `undefined` as a `void` value.
            self.cx.error(
                Diagnostic::error(
                    "`undefined` as a `void` value here is not supported yet (#930)",
                    span,
                )
                .with_note("leave the value out: `return;`, or the statement `e;` for `void e`"),
            );
            return self.error_expr(span);
        }
        let tn = self.cx.display(t);
        let core = self.cx.ty.core(t).unwrap_or(t);
        let core = self.cx.display(core);
        self.cx.error(
            Diagnostic::error(
                format!("type `{what}` is not assignable to type `{tn}` (TypeScript's TS2322)"),
                span,
            )
            .with_note(format!("make the type allow it: `{core} | {what}`")),
        );
        self.error_expr(span)
    }

    pub(crate) fn str_lit(&self, s: &str, span: Span) -> hir::Expr {
        self.mk(H::Lit(hir::Lit::Str(s.to_string())), self.cx.ty.str_, span)
    }

    pub(crate) fn intrinsic(
        &self,
        i: Intrinsic,
        args: Vec<hir::Expr>,
        ty: TyId,
        span: Span,
    ) -> hir::Expr {
        self.mk(
            H::Call {
                callee: Callee::Intrinsic(i),
                args,
            },
            ty,
            span,
        )
    }

    pub(crate) fn concat(&self, a: hir::Expr, b: hir::Expr, span: Span) -> hir::Expr {
        // Constant text on both sides (`${BASE}/users`, `PREFIX + ":"`): one literal, so a
        // module constant built from others costs nothing at each use.
        if let (Some(x), Some(y)) = (self.const_text(&a), self.const_text(&b)) {
            return self.str_lit(&format!("{x}{y}"), span);
        }
        self.intrinsic(Intrinsic::StrConcat, vec![a, b], self.cx.ty.str_, span)
    }

    /// The text of `e` when it is known: a string literal, or a module constant whose
    /// initializer is one (once checked; a module-level `let` never is).
    fn const_text<'e>(&'e self, e: &'e hir::Expr) -> Option<&'e str> {
        match &e.kind {
            H::Lit(hir::Lit::Str(s)) => Some(s),
            H::Global(d) => match &self.cx.info[d.0 as usize] {
                DefInfo::Global(g) if !g.mutable && !g.stored && g.state == BodyState::Done => {
                    match g.init.as_ref().map(|i| &i.kind) {
                        Some(H::Lit(hir::Lit::Str(s))) => Some(s),
                        _ => None,
                    }
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// The operands of `a + b` when one is a string (#740): a number or boolean on the other
    /// side (or one of those or `null`) is written as `String(x)` writes it, as in JS (`"k" + 1`
    /// is `"k1"`, `"k" + -0` is `"k0"`). Other operands are left for the mismatch report.
    pub(crate) fn concat_operands(&mut self, l: hir::Expr, r: hir::Expr) -> (hir::Expr, hir::Expr) {
        let s = self.cx.ty.str_;
        if l.ty == s && r.ty != s {
            (l, self.concat_operand(r))
        } else if r.ty == s && l.ty != s {
            (self.concat_operand(l), r)
        } else {
            (l, r)
        }
    }

    /// `x` as the non-string operand of a string `+` or `+=` ([`concat_operands`]): its
    /// `String(x)` text: a number or boolean (or one of those or `null`), an object by its
    /// `toString()`, anything else as `String(x)` writes it (a string stays itself).
    pub(crate) fn concat_operand(&mut self, h: hir::Expr) -> hir::Expr {
        let h = self.unbrand(h);
        let h = self.widen_value(h);
        let t = self.cx.ty.core(h.ty).unwrap_or(h.ty);
        // An array or tuple is written as `join()` writes it (`"" + [1, 2]` is `"1,2"`), a
        // type parameter's value as its instantiation's text, with `+`'s hint for an `any`
        // (`valueOf()` first); a symbol there throws, as in JS.
        if matches!(self.cx.ty.kind(h.ty), TyKind::Param(_)) {
            let what = "a string concatenation";
            let text = match self.param_text(h, what, Hint::Default, true) {
                Ok(text) => text,
                Err(h) => self.js_text_static(h, what),
            };
            return self.implicit_text(text);
        }
        if matches!(self.cx.ty.kind(t), TyKind::Array(_) | TyKind::Tuple(_)) && h.ty == t {
            return self.js_text(h, "a string concatenation");
        }
        let h = match self.concat_value_of(h) {
            Ok(text) => return text,
            Err(h) => h,
        };
        // An object with a `toString()` (an error, an error union): what it returns, as in JS.
        if self.writes_by_to_string(t) {
            return self.js_text(h, "a string concatenation");
        }
        // A union of primitives (`string | number`) or with an array member, or a type built
        // from type parameters (`T[K]`, `T | string`): written as a template literal writes it.
        if h.ty != self.cx.ty.str_
            && (self.primitive_union(t) || self.cx.mentions_params(t) || self.union_text_differs(t))
        {
            let text = self.implicit_any_text(h, "a string concatenation", Hint::Default);
            return self.implicit_text(text);
        }
        let ty = &self.cx.ty;
        if h.ty == ty.str_ {
            return h;
        }
        // Anything else (`string | null`, `string | string[]`, an object) as `String(x)`
        // writes it, as in a template literal (#1485).
        if !(ty.is_numeric(t) || t == ty.bool_) {
            return self.js_text(h, "a string concatenation");
        }
        let (s, span) = (ty.str_, h.span);
        self.intrinsic(Intrinsic::ToString, vec![h], s, span)
    }

    /// Can values of this type be printed / formatted (`console.log`, `${}`)? Everything but
    /// function values, interface values, `void` and shared values — also nested.
    pub(crate) fn printable(&mut self, t: TyId) -> bool {
        self.printable_depth(t, 0)
    }

    fn printable_depth(&mut self, t: TyId, depth: u32) -> bool {
        if depth > 16 {
            return true;
        }
        if self.cx.dynamic(t).is_some() {
            // Printed by the glue of the type it holds (hir_encodings.md "any and unknown").
            return true;
        }
        let parts: Vec<TyId> = match self.cx.ty.kind(t).clone() {
            TyKind::FnPtr { .. }
            | TyKind::Closure(_)
            | TyKind::Dyn(..)
            | TyKind::Unit
            | TyKind::Simd(_)
            | TyKind::Shared(_) => return false,
            // A promise prints its state and, once settled, its value (`void`: `undefined`).
            TyKind::Promise(v, e) => [v, e]
                .into_iter()
                .filter(|&p| !matches!(self.cx.ty.kind(p), TyKind::Unit))
                .collect(),
            TyKind::Adt(d, args) => {
                // A class printed through its `__inspect()` shows what that returns.
                if let Some((_, ret)) = crate::hooks::hook(self.cx, d, &args, crate::hooks::INSPECT)
                {
                    return self.printable_depth(ret, depth + 1);
                }
                // A record prints its entries (its accessors are not enumerable).
                if Some(d) == self.cx.prelude_adt("Record") {
                    return args.iter().all(|&a| self.printable_depth(a, depth + 1));
                }
                let tys: Vec<TyId> = match &self.cx.info[d.0 as usize] {
                    crate::defs::DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
                    crate::defs::DefInfo::Enum(e) => {
                        e.variants.iter().flat_map(|v| v.payload.clone()).collect()
                    }
                    _ => vec![],
                };
                tys.into_iter().map(|f| self.cx.subst(f, &args)).collect()
            }
            k => crate::types::children(&k),
        };
        parts
            .into_iter()
            .all(|p| self.printable_depth(p, depth + 1))
    }

    /// A union local read narrowed to printable members (`typeof v === "number"` on
    /// `string | i64 | f64 | () => void`, also with `| null`) as the union of just those members,
    /// so it can be printed; `h` unchanged otherwise.
    pub(crate) fn narrowed_for_print(&mut self, h: hir::Expr) -> hir::Expr {
        if self.printable(h.ty) {
            return h;
        }
        let Some(vs) = self.narrowed_variants(&h) else {
            return h;
        };
        // `T | null` with a narrowed union `T` keeps its `null` (`typeof v !== "function"`).
        let payload = self.cx.ty.opt_payload(h.ty);
        let Some(members) = self.cx.union_members(payload.unwrap_or(h.ty)) else {
            return h;
        };
        let sub: Vec<TyId> = vs.iter().map(|v| members[*v as usize]).collect();
        if sub.is_empty() || !sub.iter().all(|t| self.printable(*t)) {
            return h;
        }
        let span = h.span;
        if payload.is_some() {
            let target = self.cx.union_of(&sub, true, span);
            return self
                .option_to_option_narrowed(h, target, Some(vs))
                .unwrap_or_else(|h| h);
        }
        let target = self.cx.union_of(&sub, false, span);
        let res = if self.cx.union_def(target).is_some() {
            self.coerce_to_union(h, target)
        } else {
            self.union_to_common(h, target)
        };
        res.unwrap_or_else(|h| h)
    }

    pub(crate) fn template(
        &mut self,
        quasis: &[String],
        exprs: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let mut parts = vec![];
        for (i, q) in quasis.iter().enumerate() {
            if !q.is_empty() {
                parts.push(self.str_lit(q, span));
            }
            if let Some(e) = exprs.get(i) {
                let h = self.printed_expr(e);
                if self.reject_symbol_text(&h) {
                    parts.push(self.error_expr(h.span));
                    continue;
                }
                let part = self.implicit_any_text(h, "a template literal", Hint::String);
                parts.push(self.implicit_text(part));
            }
        }
        self.concat_parts(parts, span)
    }

    /// Template literal part `h` (a [`Self::js_text`]): a `ToString` of a value whose type
    /// mentions a type parameter becomes a `TemplateToString`, which throws for a symbol as
    /// JavaScript does (the type may be `symbol` once substituted).
    fn implicit_text(&mut self, mut h: hir::Expr) -> hir::Expr {
        // The text of a type parameter's value (`param_text`): its `ToString` branch.
        let span = h.span;
        if let H::If { cond, els, .. } = &mut h.kind {
            if is_holds_dynamic(cond) {
                let e = std::mem::replace(&mut **els, self.unit_expr(span));
                **els = self.implicit_text(e);
                return h;
            }
        }
        if let H::Call {
            callee: callee @ Callee::Intrinsic(Intrinsic::ToString),
            args,
        } = &mut h.kind
        {
            if args.len() == 1 && self.cx.mentions_params(args[0].ty) {
                *callee = Callee::Intrinsic(Intrinsic::TemplateToString);
            }
        }
        h
    }

    /// `h` as JS's `String(h)` writes it (a template literal part, `String(x)`; `what` names
    /// it in errors): a `string` itself, else an `Intrinsic::ToString` of it.
    pub(crate) fn js_text(&mut self, h: hir::Expr, what: &str) -> hir::Expr {
        let h = self.unbrand(h);
        match self.param_text(h, what, Hint::String, false) {
            Ok(text) => text,
            Err(h) => self.js_text_static(h, what),
        }
    }

    /// [`Self::js_text`] of a value whose type is known here (or a type parameter written as
    /// its instantiation's `ToString`).
    pub(crate) fn js_text_static(&mut self, h: hir::Expr, what: &str) -> hir::Expr {
        // An `any`, `unknown` or `object` is written by the value it holds (`any_text.rs`):
        // an array as JS's `Array.prototype.toString` writes it (`1,2`), an object by its
        // `toString()`, not as `console.log` prints it. So is a list of them (`any[]`), each
        // element as `String(x)` writes it (`[[1, 2], {}]` is `1,2,[object Object]`).
        let dynamic = matches!(
            self.cx.dynamic(h.ty),
            Some(Dynamic::Any | Dynamic::Unknown | Dynamic::Object)
        );
        if (dynamic || self.holds_dynamic_elems(h.ty)) && self.writes_dynamic_text() {
            if let Some(any) = self.any_ty() {
                let span = h.span;
                if h.ty == any {
                    return self.dynamic_text(h, span);
                }
                let h = self.share_if_place(h);
                let h = self.coerce(h, any);
                return self.dynamic_text(h, span);
            }
        }
        let h = match self.nullable_text(h, what) {
            Ok(text) => return text,
            Err(h) => h,
        };
        let h = self.error_union_view(h).unwrap_or_else(|h| h);
        let h = self.own_to_string(h, "toString");
        let h = self.narrowed_for_print(h);
        let t = h.ty;
        if t == self.cx.ty.str_ || self.cx.ty.is_bottom(t) {
            h
        } else if self.reject_js_list(t, what, h.span) {
            self.error_expr(h.span)
        } else if self.printable(t) {
            let hs = h.span;
            self.intrinsic(Intrinsic::ToString, vec![h], self.cx.ty.str_, hs)
        } else {
            let tn = self.cx.display(t);
            self.cx.err(
                format!("cannot format a value of type `{tn}` in {what}"),
                h.span,
            );
            self.error_expr(h.span)
        }
    }

    /// `h.<method>()` when `h` is a class or struct value with its own `method` (a template
    /// literal uses `toString()`, as in JS; `console.log` a `__inspect()` returning a
    /// `string`), else `h`. A parameter left out takes its default, and a `toString()` result
    /// that is not a `string` is written with `String(result)`, as JS does (`crate::hooks`
    /// reports the shapes this cannot call).
    pub(crate) fn own_to_string(&mut self, h: hir::Expr, method: &str) -> hir::Expr {
        let Some((d, args)) = self.adt_of(h.ty) else {
            return h;
        };
        // Declared by the class or inherited from a base class (`Error.toString`), as in JS.
        let Some(crate::collect::Found::Class { m, .. }) =
            crate::collect::lookup_method(self.cx, d, &args, method)
        else {
            return h;
        };
        crate::body::param_defaults(self.cx, m.def);
        let f = self.cx.fn_info(m.def);
        let plain = !(f.is_async || f.is_generator || f.is_async_gen);
        let to_string = method == crate::hooks::TO_STRING;
        if m.is_static
            || !plain
            || f.params.iter().any(|p| p.default.is_none())
            || (!to_string && f.ret != self.cx.ty.str_)
        {
            return h;
        }
        let ret = f.ret;
        let span = h.span;
        // A `toString()` declared to return an `any`, `unknown` or `object`: the result is tested
        // when the program runs, as `String(x)` of the instance through `any` does (an object
        // goes on to `valueOf()`, any_text.rs).
        if to_string && self.cx.dynamic(ret).is_some() && self.writes_dynamic_text() {
            if let Some(any) = self.any_ty() {
                let h = self.share_if_place(h);
                let h = self.coerce(h, any);
                return self.dynamic_text(h, span);
            }
        }
        let Some(call) = self.method_call_hir(h.clone(), method, span) else {
            return h;
        };
        if call.ty == self.cx.ty.str_ || !self.printable(call.ty) {
            return call;
        }
        let str_ = self.cx.ty.str_;
        self.intrinsic(Intrinsic::ToString, vec![call], str_, span)
    }

    /// The string parts of a template literal joined: a left fold of `StrConcat` (one part that
    /// is not a fresh value is concatenated to `""`, so the result is always a fresh string).
    pub(crate) fn concat_parts(&mut self, parts: Vec<hir::Expr>, span: Span) -> hir::Expr {
        let mut it = parts.into_iter();
        let Some(first) = it.next() else {
            return self.str_lit("", span);
        };
        let fresh = matches!(
            first.kind,
            H::Lit(_)
                | H::Call {
                    callee: Callee::Intrinsic(Intrinsic::ToString | Intrinsic::TemplateToString),
                    ..
                }
        ) || self.fresh_dynamic_text(&first)
            || matches!(&first.kind, H::If { cond, .. } if is_holds_dynamic(cond));
        let mut acc = first;
        let mut n = 1;
        for p in it {
            acc = self.concat(acc, p, span);
            n += 1;
        }
        if n == 1 && !fresh {
            // `${s}` alone: always produce a fresh owned string.
            acc = self.concat(self.str_lit("", span), acc, span);
        }
        acc.span = span;
        acc
    }
}

/// Is `e` a conditional, `&&`, `||` or `??`, whose value that is an `any` stays one where an
/// `any` is expected (`any_join`)?
fn joins_any(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(inner) => joins_any(inner),
        ast::ExprKind::Cond { .. } => true,
        ast::ExprKind::Binary { op, .. } => matches!(
            op,
            ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish
        ),
        _ => false,
    }
}

/// Is `e` a [`joins_any`] expression, or a literal holding or spreading one?
fn holds_join(e: &ast::Expr) -> bool {
    match &e.kind {
        _ if joins_any(e) => true,
        ast::ExprKind::Paren(x) | ast::ExprKind::Spread(x) => holds_join(x),
        ast::ExprKind::Array(xs) => xs.iter().any(holds_join),
        ast::ExprKind::Object(props) => props.iter().any(|p| match p {
            ast::ObjectProp::KeyValue(_, v) | ast::ObjectProp::Spread(v) => holds_join(v),
            _ => false,
        }),
        _ => false,
    }
}

/// Does the field value `e` of an object literal converted to an `any` take the `any` as its
/// expected type: the literal `null` (in parentheses or not), a conditional, `&&`, `||` or `??`
/// (whose `any` value stays one), or an object or array literal holding or spreading one?
fn dynamic_hinted(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(inner) => dynamic_hinted(inner),
        ast::ExprKind::Lit(ast::Lit::Null) => true,
        _ if joins_any(e) => true,
        ast::ExprKind::Array(xs) => xs.iter().any(dynamic_hinted),
        ast::ExprKind::Spread(x) => dynamic_hinted(x),
        ast::ExprKind::Object(props) => props.iter().any(|p| match p {
            ast::ObjectProp::KeyValue(_, v) | ast::ObjectProp::Spread(v) => dynamic_hinted(v),
            _ => false,
        }),
        _ => false,
    }
}

/// Is `e` a `HoldsDynamic` test ([`FnCx::param_text`])?
fn is_holds_dynamic(e: &hir::Expr) -> bool {
    matches!(
        e.kind,
        H::Call {
            callee: Callee::Intrinsic(Intrinsic::HoldsDynamic | Intrinsic::HoldsDynamicOrClass),
            ..
        }
    )
}
