//! `String(x)`, template literals and string `+` of an `any`, `unknown` or `object` `x`
//! (issues #924, #1435): the text of the value held, as JavaScript's `ToString(ToPrimitive(x))`
//! writes it. Each site is one call of a prelude function, `__anyString` (`String(x)` and
//! templates: hint `"string"`) or `__anyConcatText` (`+`: hint `"default"`), which keep one
//! instance each (velt_vir `track_caller.rs`); sema writes the dispatch over the program's
//! classes into their bodies, once per program:
//! - hint `"string"`: a class's `[Symbol.toPrimitive]("string")`, else its `toString()`, else
//!   (that returned an object) its `valueOf()`;
//! - hint `"default"`: `[Symbol.toPrimitive]("default")`, else its `valueOf()`, else (none, or
//!   it returned an object) its `toString()` (`Object.prototype.toString` without one).
//!
//! A result that is an object goes on to the next method, tested at run time unless its type is
//! a primitive; the last one's is Node's `TypeError`, and so is a symbol (`ToString` of it).
//! The helpers take the user's call site as their last argument (`at`, `"path:line:col"`), which
//! their stops report (velt_vir `track_caller.rs`).
//!
//! Every other value goes to the prelude's `__anyText` (`__anyConcatPlain` for `+`), which
//! writes an array as `Array.prototype.join` does, an object literal by its own `toString` /
//! `valueOf`, and another object as `Object.prototype.toString` does (`[object Object]`).

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::hir::{self, Callee, DefId, ExprKind as H, Intrinsic, TyId, TyKind, UseMode};

/// `at`: a text helper's own parameter, the user's call site its stops report.
fn site_param(span: Span) -> ast::Expr {
    let at = ast::Ident {
        name: "at".into(),
        span,
    };
    synth(ast::ExprKind::Ident(at), span)
}

/// `callee(args)`.
fn call_expr(callee: ast::Expr, args: Vec<ast::Expr>, span: Span) -> ast::Expr {
    let kind = ast::ExprKind::Call {
        callee: Box::new(callee),
        type_args: vec![],
        args,
        optional: false,
    };
    synth(kind, span)
}

/// `recv.<name>(args)` (`name` may be a symbol member's, `[Symbol.toPrimitive]`).
fn method_call(recv: ast::Expr, name: &str, args: Vec<ast::Expr>, span: Span) -> ast::Expr {
    let prop = ast::Ident {
        name: name.into(),
        span,
    };
    let member = ast::ExprKind::Member {
        object: Box::new(recv),
        prop,
        optional: false,
    };
    call_expr(synth(member, span), args, span)
}

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

/// How a class instance is written.
enum TextMethod {
    /// A `Proxy`, whose traps the text would run (not yet).
    Proxy,
    /// Its `[Symbol.toPrimitive]`, with the hint when it takes one.
    ToPrimitive(Option<&'static str>),
    /// `OrdinaryToPrimitive`: the methods tried in order, each with whether its result type is
    /// a primitive (the methods after it are never reached); `inherited`: when they all return
    /// objects, `Object.prototype.toString` writes the value (it has no `toString` of its own).
    Ordinary {
        methods: Vec<(&'static str, bool)>,
        inherited: bool,
    },
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

    /// Is this function one of the text helpers, which pass on their own `at` (the user's call
    /// site) to the helpers they call?
    fn in_text_helper(&self) -> bool {
        self.cx.scopes[self.module].is_std && TEXT_FNS.iter().any(|f| self.fn_name.ends_with(f))
    }

    /// The call site a text helper's stops report: the helper's own `at` inside one, else the
    /// location of `span` (`Intrinsic::SourceLocation`, a string constant).
    fn text_site(&mut self, span: Span) -> hir::Expr {
        if self.in_text_helper() {
            return self.expr(&site_param(span), None, Want::Borrow);
        }
        let site = H::Call {
            callee: Callee::Intrinsic(Intrinsic::SourceLocation),
            args: vec![],
        };
        let str_ = self.cx.ty.str_;
        self.mk(site, str_, span)
    }

    /// `f(args, <site>)` of the text helper `f`: `args` and the call site its stops report
    /// ([`Self::text_site`]), checked as a call with an empty string in its place.
    fn text_helper_call(&mut self, f: &str, mut args: Vec<ast::Expr>, span: Span) -> hir::Expr {
        if self.in_text_helper() {
            args.push(site_param(span));
            return self.prelude_fn_call(f, args, span);
        }
        args.push(synth(
            ast::ExprKind::Lit(ast::Lit::Str(String::new())),
            span,
        ));
        let mut call = self.prelude_fn_call(f, args, span);
        if let H::Call { args, .. } = &mut call.kind {
            if let Some(last) = args.last_mut() {
                if matches!(last.kind, H::Lit(_)) {
                    *last = self.text_site(span);
                }
            }
        }
        call
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
        if let H::Local(..) = h.kind {
            // A variable is borrowed by the call (an element or a field is shared first: what
            // the call runs may replace it).
            let mut h = h;
            set_place_mode(&mut h, UseMode::Borrow);
            let str_ = self.cx.ty.str_;
            let site = self.text_site(span);
            return self.prelude_hir_call(hint.helper(), vec![h, site], str_, span);
        }
        let h = self.share_if_place(h);
        self.with_temp("<text>", h, span, span, |lw, v| {
            lw.text_helper_call(hint.helper(), vec![v.clone()], span)
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
                (hint.fallback(), vec![site_param(span)]),
                |lw, c| lw.text_method(c, hint).is_some(),
                |lw, c, read| match lw.text_method(c, hint) {
                    Some(m) => lw.text_method_call(&m, read, span),
                    None => lw.error_expr(span),
                },
                span,
            )
        })
    }

    /// The text of the class instance `read` (inside a text helper, whose `at` its stops
    /// report) as `m` says: `read[Symbol.toPrimitive](hint)`, or its `OrdinaryToPrimitive`
    /// methods in turn (`__anyPrimTry(read.valueOf(), at) ?? __anyPrimText(read.toString(), at)`),
    /// the result written with `ToString`.
    fn text_method_call(&mut self, m: &TextMethod, read: ast::Expr, span: Span) -> hir::Expr {
        let at = || site_param(span);
        let call_fn = |f: &str, args: Vec<ast::Expr>| {
            let callee = synth(
                ast::ExprKind::Ident(ast::Ident {
                    name: f.into(),
                    span,
                }),
                span,
            );
            call_expr(callee, args, span)
        };
        let text = match m {
            TextMethod::Proxy => {
                let lit = synth(ast::ExprKind::Lit(ast::Lit::Str("Proxy".into())), span);
                call_fn("__anyTextUnsupported", vec![lit, at()])
            }
            TextMethod::ToPrimitive(hint) => {
                let args = hint
                    .map(|h| synth(ast::ExprKind::Lit(ast::Lit::Str(h.into())), span))
                    .into_iter()
                    .collect();
                let call = method_call(read, "[Symbol.toPrimitive]", args, span);
                call_fn("__anyPrimText", vec![call, at()])
            }
            TextMethod::Ordinary { methods, inherited } => {
                let mut parts = vec![];
                let mut ended = false;
                for (i, &(name, primitive)) in methods.iter().enumerate() {
                    let call = method_call(read.clone(), name, vec![], span);
                    if primitive || (i + 1 == methods.len() && !inherited) {
                        parts.push(call_fn("__anyPrimText", vec![call, at()]));
                        ended = true;
                        break;
                    }
                    parts.push(call_fn("__anyPrimTry", vec![call, at()]));
                }
                if !ended {
                    parts.push(call_fn("__anyText", vec![read, at()]));
                }
                let mut parts = parts.into_iter().rev();
                let Some(mut text) = parts.next() else {
                    return self.error_expr(span);
                };
                for p in parts {
                    let kind = ast::ExprKind::Binary {
                        op: ast::BinaryOp::Nullish,
                        lhs: Box::new(p),
                        rhs: Box::new(text),
                    };
                    text = synth(kind, span);
                }
                text
            }
        };
        self.expr(&text, None, Want::Move)
    }

    /// The method an instance of class `c` (its own or a base class's) is written by with
    /// `hint`, if any.
    fn text_method(&mut self, c: DefId, hint: Hint) -> Option<TextMethod> {
        if self.cx.prelude_adt("Proxy") == Some(c) {
            return Some(TextMethod::Proxy);
        }
        let n = self.cx.adt(c).map_or(0, |a| a.generics.len());
        let params: Vec<_> = (0..n as u32).map(|i| self.cx.ty.param(i)).collect();
        let to_primitive = self.callable_method(c, &params, "[Symbol.toPrimitive]");
        if let Some((_, arity)) = to_primitive.filter(|&(_, arity)| arity <= 1) {
            return Some(TextMethod::ToPrimitive((arity == 1).then(|| hint.name())));
        }
        let method = |lw: &mut Self, name: &'static str| {
            let (ret, arity) = lw.callable_method(c, &params, name)?;
            (arity == 0).then(|| (name, lw.primitive_ty(ret)))
        };
        let to_string = method(self, "toString");
        // A `Date`'s `[Symbol.toPrimitive]` writes it as a string for the `"default"` hint too.
        let date = self
            .cx
            .prelude_adt("Date")
            .is_some_and(|d| self.cx.class_extends(c, d));
        let value_of = match date {
            true => None,
            false => method(self, "valueOf"),
        };
        let (methods, inherited) = match hint {
            // Without a `toString` of its own, `Object.prototype.toString` writes it.
            Hint::String => (std::iter::once(to_string?).chain(value_of).collect(), false),
            Hint::Default => {
                let methods: Vec<_> = value_of.into_iter().chain(to_string).collect();
                if methods.is_empty() {
                    return None;
                }
                (methods, to_string.is_none())
            }
        };
        Some(TextMethod::Ordinary { methods, inherited })
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
            lw.text_helper_call("__anyJoin", vec![v.clone(), sep], span)
        })
    }

    /// Does `t` hold `any`, `unknown` or `object` values in its arrays or tuples (`any[]`,
    /// `object[][]`, `[any, number]`, also `any[] | null`)? Such a list is written as
    /// JavaScript's `join` writes it (`__anyJoin`), each element as `String(x)` writes it, not
    /// as `console.log` prints it.
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
