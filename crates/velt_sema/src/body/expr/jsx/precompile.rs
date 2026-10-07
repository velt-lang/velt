//! SSR precompile (docs/contracts/jsx.md "SSR precompile"): when the runtime exports
//! `jsxTemplate`, `jsxEscape`, `jsxAttr` and `Text`, every maximal tree of intrinsic elements
//! becomes one `jsxTemplate(strings, slots)` call. Static tags, attributes and text are escaped
//! at compile time; a dynamic child assignable to `JSX.Text` and every dynamic attribute are
//! folded into the surrounding string as a template literal (`` `<td>${jsxEscape(x)}</td>` ``).
//! Only `Element` parts are slots: components, fragments, elements that are not precompiled (a
//! spread or a `key`), `Element`-typed expressions as they are, and any other child as
//! `Fragment([v], null)`.
//!
//! With a `jsxTextSeparator` export, the separator goes between two adjacent text children of
//! one element ("Text separator" in the contract). Adjacent text children are collected as a
//! run and written out when markup or a slot ends it: a separator between two parts that are
//! text whatever their values is part of the constant string; a part that may be `null` or a
//! boolean (a boundary, not text) decides at run time, so the run's values are bound to
//! temporaries in order and the separator is a conditional.

use velt_common::Span;
use velt_syntax::ast;

use super::attrs::Fixed;
use super::children::real_children;
use super::escape::escape_html;
use super::intrinsic_tag;
use super::key::is_key;
use super::provider::{Precompile, Provider};
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, LogicOp, PatKind as P};

/// HTML void elements: written without a closing tag.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// A template being built: the parts of the current string (static text is merged), the
/// finished strings and the slots between them.
#[derive(Default)]
struct Template {
    strings: Vec<hir::Expr>,
    /// Parts of the current string: static text and `string`-typed calls.
    parts: Vec<hir::Expr>,
    /// Static text not yet added to `parts`.
    text: String,
    slots: Vec<hir::Expr>,
    /// The provider's `jsxTextSeparator`.
    sep: Option<String>,
    /// With a separator: the adjacent text children not written yet.
    run: Vec<TextPart>,
}

/// A text child in a run.
enum TextPart {
    /// Escaped static text.
    Static(String),
    /// A value converted to `JSX.Text`, written with `jsxEscape`.
    Value(hir::Expr, Textness),
}

/// Is a child value text (rather than `true`, `false` or `null`, which are boundaries)?
#[derive(Clone, Copy, PartialEq, Eq)]
enum Textness {
    Always,
    Never,
    /// Decided at run time: the value's type has a boolean or `null` member and another one.
    Maybe,
}

/// Is `tag` an HTML void element?
pub(super) fn is_void(tag: &str) -> bool {
    VOID_ELEMENTS.contains(&tag)
}

/// Can intrinsic element `el` (tag `tag`) be written into a template?
pub(super) fn precompilable(el: &ast::JsxElement, tag: &str) -> bool {
    let plain_attrs = !el
        .attrs
        .iter()
        .any(|a| matches!(a, ast::JsxAttr::Spread { .. }) || is_key(a));
    plain_attrs && (!is_void(tag) || real_children(&el.children).is_empty())
}

impl FnCx<'_, '_> {
    /// `jsxTemplate([...strings], [...slots])` for precompilable intrinsic element `el`.
    pub(super) fn jsx_template(
        &mut self,
        p: &Provider,
        pc: Precompile,
        el: &ast::JsxElement,
        tag: &str,
        tag_span: Span,
    ) -> hir::Expr {
        let mut t = Template {
            sep: p.text_separator.clone(),
            ..Template::default()
        };
        if !self.template_element(p, pc, el, tag, tag_span, &mut t) {
            return self.error_expr(el.span);
        }
        self.end_string(p, pc, &mut t, el.span);
        let str_ = self.cx.ty.str_;
        let strings_ty = self.cx.ty.array(str_);
        let strings = self.mk(H::ArrayLit(t.strings), strings_ty, el.span);
        let slots_ty = self.cx.ty.array(p.element);
        let slots = self.mk(H::ArrayLit(t.slots), slots_ty, el.span);
        self.jsx_call(p, pc.template, "jsxTemplate", vec![strings, slots], el.span)
    }

    /// Write element `el` into `t`; false if its tag is unknown (reported).
    fn template_element(
        &mut self,
        p: &Provider,
        pc: Precompile,
        el: &ast::JsxElement,
        tag: &str,
        tag_span: Span,
        t: &mut Template,
    ) -> bool {
        // Precompiled elements have no spreads, so nothing is bound here.
        let mut lets = vec![];
        let Some(attrs) = self.intrinsic_attrs(p, el, tag, tag_span, &mut lets) else {
            self.jsx_loose(p, el);
            return false;
        };
        self.end_run(p, pc, t, el.span);
        t.text.push('<');
        t.text.push_str(tag);
        for a in attrs {
            match a.fixed {
                Some(Fixed::Text(s)) => {
                    t.text
                        .push_str(&format!(" {}=\"{}\"", a.name, escape_html(&s)));
                }
                Some(Fixed::Bare) => t.text.push_str(&format!(" {}", a.name)),
                None => {
                    let span = a.value.span;
                    let name = self.str_lit(&a.name, span);
                    let h = self.jsx_call(p, pc.attr, "jsxAttr", vec![name, a.value], span);
                    self.add_part(t, h);
                }
            }
        }
        t.text.push('>');
        if is_void(tag) {
            return true;
        }
        for c in &el.children {
            self.template_child(p, pc, c, t);
        }
        self.end_run(p, pc, t, el.span);
        t.text.push_str(&format!("</{tag}>"));
        true
    }

    fn template_child(
        &mut self,
        p: &Provider,
        pc: Precompile,
        c: &ast::JsxChild,
        t: &mut Template,
    ) {
        match c {
            ast::JsxChild::Text { value, .. } => {
                let text = escape_html(value);
                if t.sep.is_some() {
                    t.run.push(TextPart::Static(text));
                } else {
                    t.text.push_str(&text);
                }
            }
            ast::JsxChild::Expr { expr: None, .. } => {}
            ast::JsxChild::Expr { span, .. } | ast::JsxChild::Spread { span, .. } => {
                let h = self.child_value(p, c, p.child);
                self.dynamic_child(p, pc, h, *span, t);
            }
            ast::JsxChild::Element(inner) => {
                let tag = inner.name.as_ref().and_then(intrinsic_tag);
                match tag {
                    Some(tag) if precompilable(inner, &tag) => {
                        let tag_span = inner.name.as_ref().map_or(inner.span, |n| n.span());
                        if !self.template_element(p, pc, inner, &tag, tag_span, t) {
                            let e = self.error_expr(inner.span);
                            self.add_slot(p, pc, t, e);
                        }
                    }
                    _ => {
                        let h = self.jsx_element(p, inner);
                        self.add_slot(p, pc, t, h);
                    }
                }
            }
        }
    }

    /// `{expr}`: escaped text when it is a `JSX.Text`, a slot when it is an `Element`, else a
    /// slot `Fragment([v], null)`.
    fn dynamic_child(
        &mut self,
        p: &Provider,
        pc: Precompile,
        h: hir::Expr,
        span: Span,
        t: &mut Template,
    ) {
        if self.cx.ty.is_bottom(h.ty) || self.compatible(p.element, h.ty) {
            let h = self.coerce(h, p.element);
            return self.add_slot(p, pc, t, h);
        }
        let textness = self.textness(&h);
        match self.try_coerce(h, pc.text) {
            Ok(h) if t.sep.is_some() => t.run.push(TextPart::Value(h, textness)),
            Ok(h) => {
                let s = self.escape_call(p, pc, h, span);
                self.add_part(t, s);
            }
            Err(h) => {
                let child = self.as_child(p, h);
                let ty = self.cx.ty.array(p.child);
                let children = self.mk(H::ArrayLit(vec![child]), ty, span);
                let opt_str = self.cx.ty.option(self.cx.ty.str_);
                let key = self.mk(H::Lit(hir::Lit::Null), opt_str, span);
                let frag = self.jsx_call(p, p.fragment, "Fragment", vec![children, key], span);
                self.add_slot(p, pc, t, frag);
            }
        }
    }

    /// Whether child value `h` (before its conversion to `JSX.Text`) is text.
    fn textness(&mut self, h: &hir::Expr) -> Textness {
        if matches!(h.kind, H::Lit(hir::Lit::Null | hir::Lit::Bool(_))) {
            return Textness::Never;
        }
        let (inner, nullable) = match self.cx.ty.opt_payload(h.ty) {
            Some(inner) => (inner, true),
            None => (h.ty, false),
        };
        let members = self.cx.union_members(inner).unwrap_or_else(|| vec![inner]);
        let bools = members
            .iter()
            .filter(|m| self.cx.typeof_tag(**m) == "boolean")
            .count();
        if bools == members.len() {
            Textness::Never
        } else if nullable || bools > 0 {
            Textness::Maybe
        } else {
            Textness::Always
        }
    }

    /// A `string`-typed part of the current template string.
    fn add_part(&mut self, t: &mut Template, h: hir::Expr) {
        self.flush_text(t, h.span);
        t.parts.push(h);
    }

    /// An `Element` slot: ends the current string.
    fn add_slot(&mut self, p: &Provider, pc: Precompile, t: &mut Template, h: hir::Expr) {
        let span = h.span;
        self.end_string(p, pc, t, span);
        t.slots.push(h);
    }

    fn flush_text(&mut self, t: &mut Template, span: Span) {
        if !t.text.is_empty() {
            let text = std::mem::take(&mut t.text);
            t.parts.push(self.str_lit(&text, span));
        }
    }

    /// Finish the current string (a literal, or a template literal of its parts).
    fn end_string(&mut self, p: &Provider, pc: Precompile, t: &mut Template, span: Span) {
        self.end_run(p, pc, t, span);
        self.flush_text(t, span);
        let parts = std::mem::take(&mut t.parts);
        let s = match parts.len() {
            0 => self.str_lit("", span),
            1 if matches!(parts[0].kind, H::Lit(_)) => {
                parts.into_iter().next().expect("ICE: one part")
            }
            _ => self.concat_parts(parts, span),
        };
        t.strings.push(s);
    }

    /// Write the pending text run with separators between adjacent text parts.
    fn end_run(&mut self, p: &Provider, pc: Precompile, t: &mut Template, span: Span) {
        if t.run.is_empty() {
            return;
        }
        let run = std::mem::take(&mut t.run);
        let sep = t.sep.clone().expect("ICE: a text run without a separator");
        let textness = |part: &TextPart| match part {
            TextPart::Static(_) => Textness::Always,
            TextPart::Value(_, x) => *x,
        };
        let runtime = run.windows(2).any(|w| {
            let (a, b) = (textness(&w[0]), textness(&w[1]));
            a != Textness::Never
                && b != Textness::Never
                && (a, b) != (Textness::Always, Textness::Always)
        });
        if !runtime {
            // Every separator is known now: constant text between the parts.
            let mut prev = Textness::Never;
            for part in run {
                let cur = textness(&part);
                if prev == Textness::Always && cur == Textness::Always {
                    t.text.push_str(&sep);
                }
                prev = cur;
                match part {
                    TextPart::Static(s) => t.text.push_str(&s),
                    TextPart::Value(h, _) => {
                        let span = h.span;
                        let s = self.escape_call(p, pc, h, span);
                        self.add_part(t, s);
                    }
                }
            }
            return;
        }
        // `{ const t1 = v1; …; `…${t1 is text && t2 is text ? sep : ""}…` }`: the values are
        // read once, in source order.
        let mut lets = vec![];
        let mut parts: Vec<hir::Expr> = vec![];
        let mut text = String::new();
        // What precedes the current part: `None` for a boundary, `Some(None)` for text,
        // `Some(Some(test))` for text when `test` is true.
        let mut prev: Option<Option<hir::Expr>> = None;
        for part in run {
            let (cur, value) = match part {
                TextPart::Static(s) => (Some(None), Err(s)),
                TextPart::Value(h, x) => {
                    let v = self.temp("text", h, &mut lets);
                    let cur = match x {
                        Textness::Always => Some(None),
                        Textness::Never => None,
                        Textness::Maybe => Some(Some(self.is_text_test(&v, span))),
                    };
                    (cur, Ok(v))
                }
            };
            match (prev.take(), &cur) {
                (Some(None), Some(None)) => text.push_str(&sep),
                (Some(a), Some(b)) => {
                    let cond = match (a, b.clone()) {
                        (Some(a), Some(b)) => self.mk(
                            H::Logical {
                                op: LogicOp::And,
                                lhs: Box::new(a),
                                rhs: Box::new(b),
                            },
                            self.cx.ty.bool_,
                            span,
                        ),
                        (Some(c), None) | (None, Some(c)) => c,
                        (None, None) => unreachable!("ICE: a constant separator"),
                    };
                    if !text.is_empty() {
                        parts.push(self.str_lit(&std::mem::take(&mut text), span));
                    }
                    let then = self.str_lit(&sep, span);
                    let els = self.str_lit("", span);
                    let kind = H::If {
                        cond: Box::new(cond),
                        then: Box::new(then),
                        els: Box::new(els),
                    };
                    parts.push(self.mk(kind, self.cx.ty.str_, span));
                }
                _ => {}
            }
            prev = cur;
            match value {
                Err(s) => text.push_str(&s),
                Ok(v) => {
                    if !text.is_empty() {
                        parts.push(self.str_lit(&std::mem::take(&mut text), span));
                    }
                    let vspan = v.span;
                    parts.push(self.escape_call(p, pc, v, vspan));
                }
            }
        }
        if !text.is_empty() {
            parts.push(self.str_lit(&text, span));
        }
        let s = self.concat_parts(parts, span);
        let block = self.with_lets(lets, s);
        self.add_part(t, block);
    }

    /// `jsxEscape(h)`.
    fn escape_call(&mut self, p: &Provider, pc: Precompile, h: hir::Expr, span: Span) -> hir::Expr {
        self.jsx_call(p, pc.escape, "jsxEscape", vec![h], span)
    }

    /// Is `v` (a `JSX.Text` temporary) text: not `null` and not a boolean?
    fn is_text_test(&mut self, v: &hir::Expr, span: Span) -> hir::Expr {
        let mut alts = vec![];
        for (pat, ty) in self.member_patterns(v.ty, span) {
            if ty.is_some_and(|ty| self.cx.typeof_tag(ty) != "boolean") {
                alts.push(pat);
            }
        }
        let yes = match alts.len() {
            0 => None,
            1 => alts.pop(),
            _ => Some(self.pat(P::Or(alts), v.ty, span)),
        };
        self.bool_match(v.clone(), yes, span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn void_elements() {
        assert!(is_void("br"));
        assert!(is_void("img"));
        assert!(!is_void("div"));
        assert!(!is_void("my-br"));
    }
}
