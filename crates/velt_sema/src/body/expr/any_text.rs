//! `String(x)`, template literals and string `+` of an `any`, `unknown` or `object` `x`
//! (issues #924, #1435): the text of the value held, as JavaScript's `ToString(ToPrimitive(x))`
//! writes it. Each site is one call of a prelude function, `__anyString` (`String(x)` and
//! templates: hint `"string"`) or `__anyConcatText` (`+`: hint `"default"`), which keep one
//! instance each (velt_vir `track_caller.rs`); sema writes the dispatch over the program's
//! classes into their bodies, once per program:
//! - hint `"string"`: a class's `[Symbol.toPrimitive]("string")`, else its `toString()`;
//! - hint `"default"`: `[Symbol.toPrimitive]("default")`, else a `valueOf()` returning a
//!   primitive, else `toString()`.
//!
//! Every other value goes to the prelude's `__anyText` (`__anyConcatPlain` for `+`), which
//! writes an array as `Array.prototype.join` does, an object literal by its own `toString` /
//! `valueOf`, and another object as `Object.prototype.toString` does (`[object Object]`).

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::body::places::{is_place, set_place_mode};
use crate::body::{FnCx, Want};
use crate::hir::{self, Callee, DefId, ExprKind as H, TyId, TyKind, UseMode};

/// The hint of a `ToPrimitive` (JavaScript's `OrdinaryToPrimitive` order).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hint {
    /// `String(x)` and template literals.
    String,
    /// `+` and `+=`.
    Default,
}

impl Hint {
    /// The prelude function writing a value with this hint, whose body holds the dispatch.
    fn helper(self) -> &'static str {
        match self {
            Hint::String => "__anyString",
            Hint::Default => "__anyConcatText",
        }
    }

    /// The prelude function for values that are no class instances with such a method.
    fn fallback(self) -> &'static str {
        match self {
            Hint::String => "__anyText",
            Hint::Default => "__anyConcatPlain",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Hint::String => "string",
            Hint::Default => "default",
        }
    }
}

/// The prelude functions whose `String(x)` of an `any` is the dynamic text (std code otherwise
/// writes an `any` as `console.log` prints it).
const TEXT_FNS: [&str; 6] = [
    "__anyText",
    "__anyString",
    "__anyConcatText",
    "__anyConcatPlain",
    "__anyPrimText",
    "__anyJoinSeen",
];

/// The method a class instance is written by: its name and the hint passed
/// (`[Symbol.toPrimitive]`).
struct TextMethod {
    name: &'static str,
    hint: Option<&'static str>,
}

impl FnCx<'_, '_> {
    /// Does this function write an `any` by the value it holds (user code, the text helpers)?
    pub(crate) fn writes_dynamic_text(&self) -> bool {
        !self.cx.scopes[self.module].is_std || TEXT_FNS.iter().any(|f| self.fn_name.ends_with(f))
    }

    /// The text of the dynamic value `h` (an `any`) as `String(h)` writes it.
    pub(crate) fn dynamic_text(&mut self, h: hir::Expr, span: Span) -> hir::Expr {
        self.dynamic_text_as(h, span, Hint::String)
    }

    /// The text of the dynamic value `h` (an `any`) with `hint`: a call of the hint's prelude
    /// function (in that function's body, the dispatch itself).
    pub(crate) fn dynamic_text_as(&mut self, h: hir::Expr, span: Span, hint: Hint) -> hir::Expr {
        if self.cx.scopes[self.module].is_std {
            for own in [Hint::String, Hint::Default] {
                if self.fn_name.ends_with(own.helper()) {
                    return self.text_dispatch(h, span, own);
                }
            }
        }
        // The helpers read an array's elements and an object's keys through `any` (their own
        // uses don't count: they run only where a program's code calls them).
        self.note_dyn_members();
        if is_place(&h) {
            // Borrowed by the call, as a variable passed for a parameter is.
            let mut h = h;
            set_place_mode(&mut h, UseMode::Borrow);
            let str_ = self.cx.ty.str_;
            return self.prelude_hir_call(hint.helper(), vec![h], str_, span);
        }
        self.with_temp("<text>", h, span, span, |lw, v| {
            lw.prelude_fn_call(hint.helper(), vec![v.clone()], span)
        })
    }

    /// Is `h` (a part of a template literal) a fresh string: a call of a text helper, or a
    /// block or match all of whose values are one?
    pub(crate) fn fresh_dynamic_text(&mut self, h: &hir::Expr) -> bool {
        match &h.kind {
            H::Call {
                callee: Callee::Def(d, _),
                ..
            } => TEXT_FNS
                .iter()
                .chain(&["__anyJoin"])
                .any(|f| self.cx.prelude_fn(f) == Some(*d)),
            H::Block(b) => b.value.as_ref().is_some_and(|v| self.fresh_dynamic_text(v)),
            H::Match { arms, .. } => arms.iter().all(|a| self.fresh_dynamic_text(&a.body)),
            _ => false,
        }
    }

    /// `match (h) { InstanceOf(C) => <C's text method>, ..., _ => <fallback>(h) }` over the
    /// classes with a method for `hint`.
    fn text_dispatch(&mut self, h: hir::Expr, span: Span, hint: Hint) -> hir::Expr {
        let init = self.share_if_place(h);
        self.with_temp("<text>", init, span, span, |lw, v| {
            let s = lw.expr(v, None, Want::Borrow);
            let str_ = lw.cx.ty.str_;
            lw.dynamic_class_match_where(
                s,
                str_,
                hint.fallback(),
                |lw, c| lw.text_method(c, hint).is_some(),
                |lw, c, read| match lw.text_method(c, hint) {
                    Some(m) => lw.text_method_call(&m, read, span),
                    None => lw.error_expr(span),
                },
                span,
            )
        })
    }

    /// `read.<m>()` (`read[Symbol.toPrimitive](hint)`) through the `any` `read` (a generic
    /// class's instance has no type arguments here), its result written with `ToString`.
    fn text_method_call(&mut self, m: &TextMethod, read: ast::Expr, span: Span) -> hir::Expr {
        let prop = ast::Ident {
            name: m.name.into(),
            span,
        };
        let member = synth(
            ast::ExprKind::Member {
                object: Box::new(read),
                prop,
                optional: false,
            },
            span,
        );
        let args = m
            .hint
            .map(|h| synth(ast::ExprKind::Lit(ast::Lit::Str(h.into())), span))
            .into_iter()
            .collect();
        let call = synth(
            ast::ExprKind::Call {
                callee: Box::new(member),
                type_args: vec![],
                args,
                optional: false,
            },
            span,
        );
        self.prelude_fn_call("__anyPrimText", vec![call], span)
    }

    /// The method an instance of class `c` (its own or a base class's) is written by with
    /// `hint`, if any.
    fn text_method(&mut self, c: DefId, hint: Hint) -> Option<TextMethod> {
        let n = self.cx.adt(c).map_or(0, |a| a.generics.len());
        let params: Vec<_> = (0..n as u32).map(|i| self.cx.ty.param(i)).collect();
        let to_primitive = self.callable_method(c, &params, "[Symbol.toPrimitive]");
        if let Some((_, arity)) = to_primitive.filter(|&(_, arity)| arity <= 1) {
            return Some(TextMethod {
                name: "[Symbol.toPrimitive]",
                hint: (arity == 1).then(|| hint.name()),
            });
        }
        // A `Date`'s `[Symbol.toPrimitive]` writes it as a string for the `"default"` hint too.
        let date = self
            .cx
            .prelude_adt("Date")
            .is_some_and(|d| self.cx.class_extends(c, d));
        if hint == Hint::Default && !date {
            if let Some((ret, 0)) = self.callable_method(c, &params, "valueOf") {
                if self.primitive_ty(ret) {
                    return Some(TextMethod {
                        name: "valueOf",
                        hint: None,
                    });
                }
            }
        }
        self.callable_method(c, &params, "toString")
            .filter(|&(_, arity)| arity == 0)?;
        Some(TextMethod {
            name: "toString",
            hint: None,
        })
    }

    /// The result type of class `c`'s instance method `name` (its own or inherited) and how
    /// many arguments it takes: 0 when every parameter has a default, else its parameter count.
    fn callable_method(&mut self, c: DefId, params: &[TyId], name: &str) -> Option<(TyId, usize)> {
        let Some(crate::collect::Found::Class { m, .. }) =
            crate::collect::lookup_method(self.cx, c, params, name)
        else {
            return None;
        };
        if m.is_static {
            return None;
        }
        crate::body::param_defaults(self.cx, m.def);
        let f = self.cx.fn_info(m.def);
        if f.is_async || f.is_generator || f.is_async_gen {
            return None;
        }
        let arity = match f.params.iter().all(|p| p.default.is_some()) {
            true => 0,
            false => f.params.len(),
        };
        Some((f.ret, arity))
    }

    /// Is `t` a `string`, number or boolean (a `valueOf()` result `ToPrimitive` keeps)?
    fn primitive_ty(&mut self, t: TyId) -> bool {
        let w = self.cx.widened(t);
        let ty = &self.cx.ty;
        w == ty.str_ || w == ty.bool_ || ty.is_numeric(w)
    }

    /// Is `xs.join(sep)` of an `xs` of type `t` [`Self::dynamic_list_join`]'s?
    pub(crate) fn joins_dynamic(&mut self, t: TyId) -> bool {
        matches!(self.cx.ty.kind(t), TyKind::Array(_))
            && self.holds_dynamic_elems(t)
            && self.writes_dynamic_text()
    }

    /// `xs.join(sep)` of an array holding `any`, `unknown` or `object` values (`joins_dynamic`):
    /// the prelude's `__anyJoin(xs, sep)`, which writes each element as `String(x)` writes it.
    pub(crate) fn dynamic_list_join(
        &mut self,
        recv: hir::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        self.note_dyn_members();
        let init = self.share_if_place(recv);
        let init = self.coerce(init, any);
        self.with_temp("<list>", init, span, span, |lw, v| {
            let comma = || synth(ast::ExprKind::Lit(ast::Lit::Str(",".into())), span);
            let sep = args.first().cloned().unwrap_or_else(comma);
            lw.prelude_fn_call("__anyJoin", vec![v.clone(), sep], span)
        })
    }

    /// Does `t` hold `any`, `unknown` or `object` values in its arrays or tuples (`any[]`,
    /// `object[][]`, `[any, number]`, also `any[] | null`)? Such a list is written as JavaScript's `join` writes it
    /// (`__anyJoin`), each element as `String(x)` writes it, not as `console.log` prints it.
    pub(crate) fn holds_dynamic_elems(&mut self, t: TyId) -> bool {
        self.holds_dynamic_depth(t, 0)
    }

    fn holds_dynamic_depth(&mut self, t: TyId, depth: u32) -> bool {
        if depth > 8 {
            return false;
        }
        let elems = match self.cx.ty.kind(t) {
            // `any[] | null`: written as the array or `null`.
            TyKind::Option(inner, _) => return self.holds_dynamic_depth(*inner, depth + 1),
            TyKind::Array(e) => vec![*e],
            TyKind::Tuple(es) => es.clone(),
            _ => return false,
        };
        elems
            .into_iter()
            .any(|e| self.cx.dynamic(e).is_some() || self.holds_dynamic_depth(e, depth + 1))
    }
}
