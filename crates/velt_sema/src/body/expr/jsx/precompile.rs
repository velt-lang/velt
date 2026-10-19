//! SSR precompile (docs/internals/contracts/jsx.md "SSR precompile"): when the runtime exports
//! `jsxTemplate`, `jsxEscape`, `jsxAttr` and `Text`, every maximal tree of intrinsic elements
//! becomes one `jsxTemplate(strings, slots)` call. Static tags, attributes and text are escaped
//! at compile time; a dynamic child assignable to `JSX.Text` and every dynamic attribute are
//! folded into the surrounding string as a template literal (`` `<td>${jsxEscape(x)}</td>` ``).
//! Only `Element` parts are slots: components, fragments, elements that are not precompiled (a
//! spread or a `key`), `Element`-typed expressions as they are, and any other child as
//! `Fragment([v], null)`. A tree without slots is `jsxTemplateString(html)` when the runtime
//! exports it, and a `string` child is `jsxEscapeString(x)` when it exports that.
//!
//! With a `jsxTextSeparator` export, text children are collected as runs (`text_run`); with a
//! `jsxSoleEmpty` export, an element's only `{expr}` child may render differently (`sole_child`);
//! with a `jsxList` export, `{xs.map((x) => <tr>…</tr>)}` builds strings (`list_fold`).

use velt_common::Span;
use velt_syntax::ast;

use super::attrs::Fixed;
use super::children::real_children;
use super::escape::escape_html;
use super::intrinsic_tag;
use super::key::is_key;
use super::provider::{Precompile, Provider};
use super::text_run::{TextPart, Textness};
use crate::body::places::set_place_mode;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, UseMode};

/// HTML void elements: written without a closing tag, unless the provider lists its own
/// (`jsxVoidElements`, [`Provider::is_void`]).
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// A template being built: the parts of the current string (static text is merged), the
/// finished strings and the slots between them.
#[derive(Default)]
pub(super) struct Template {
    pub(super) strings: Vec<hir::Expr>,
    /// Parts of the current string: static text and `string`-typed calls.
    pub(super) parts: Vec<hir::Expr>,
    /// Static text not yet added to `parts`.
    pub(super) text: String,
    pub(super) slots: Vec<hir::Expr>,
    /// The provider's `jsxTextSeparator`.
    pub(super) sep: Option<String>,
    /// With a separator: the adjacent text children not written yet.
    pub(super) run: Vec<TextPart>,
}

/// Is `tag` an HTML void element?
pub(super) fn is_html_void(tag: &str) -> bool {
    VOID_ELEMENTS.contains(&tag)
}

impl Provider {
    /// Is `tag` written without an end tag: one the provider lists in `jsxVoidElements`, or an
    /// HTML void element when it lists none?
    pub(super) fn is_void(&self, tag: &str) -> bool {
        match &self.void_elements {
            Some(list) => list.iter().any(|v| v == tag),
            None => is_html_void(tag),
        }
    }
}

/// Can intrinsic element `el` (tag `tag`) be written into a template?
pub(super) fn precompilable(p: &Provider, el: &ast::JsxElement, tag: &str) -> bool {
    let plain_attrs = !el
        .attrs
        .iter()
        .any(|a| matches!(a, ast::JsxAttr::Spread { .. }) || is_key(a));
    plain_attrs && (!p.is_void(tag) || real_children(&el.children).is_empty())
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
        let fold = self.jsx_list_fold == Some(el.span);
        if fold {
            self.jsx_list_fold = None;
        }
        let mut t = Template {
            sep: p.text_separator.clone(),
            ..Template::default()
        };
        if !self.template_element(p, pc, el, tag, tag_span, &mut t) {
            return self.error_expr(el.span);
        }
        self.end_string(p, pc, &mut t, el.span);
        if fold && t.slots.is_empty() {
            // A row of a folded list: its markup is the closure's result (`list_fold`).
            self.jsx_list_folded = true;
            return t.strings.pop().expect("ICE: one string without slots");
        }
        if let (Some(f), true) = (pc.template_string, t.slots.is_empty()) {
            let [html] =
                <[hir::Expr; 1]>::try_from(t.strings).expect("ICE: one string without slots");
            return self.jsx_call(p, f, "jsxTemplateString", vec![html], el.span);
        }
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
        self.end_run(p, pc, t, el.span, false);
        t.text.push('<');
        t.text.push_str(tag);
        for a in attrs {
            match a.fixed {
                // `'` is the provider's to escape (`&#39;`, `&#x27;`): `jsxAttr` below.
                Some(Fixed::Text(s)) if !s.contains('\'') => {
                    t.text
                        .push_str(&format!(" {}=\"{}\"", a.name, escape_html(&s)));
                }
                Some(Fixed::Bare) => t.text.push_str(&format!(" {}", a.name)),
                _ => {
                    let span = a.value.span;
                    let name = self.str_lit(&a.name, span);
                    let h = self.jsx_call(p, pc.attr, "jsxAttr", vec![name, a.value], span);
                    self.add_part(t, h);
                }
            }
        }
        t.text.push('>');
        if p.is_void(tag) {
            return true;
        }
        match (&p.sole_empty, real_children(&el.children).as_slice()) {
            (
                Some(sole),
                [c @ (ast::JsxChild::Expr { expr: Some(_), .. } | ast::JsxChild::Spread { .. })],
            ) => {
                self.sole_child(p, pc, c, sole, t);
            }
            _ => {
                for c in &el.children {
                    self.template_child(p, pc, c, t);
                }
            }
        }
        self.end_run(p, pc, t, el.span, false);
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
            ast::JsxChild::Text { value, span } if value.contains('\'') => {
                // The provider escapes `'` its own way (`&#39;`, `&#x27;`): `jsxEscape`, one text
                // part like static text.
                let h = self.str_lit(value, *span);
                if t.sep.is_some() {
                    t.run.push(TextPart::Value(h, Textness::Always, false));
                } else {
                    let s = self.escape_call(p, pc, h, *span);
                    self.add_part(t, s);
                }
            }
            ast::JsxChild::Text { value, .. } => {
                let text = escape_html(value);
                if t.sep.is_some() {
                    t.run.push(TextPart::Static(text));
                } else {
                    t.text.push_str(&text);
                }
            }
            ast::JsxChild::Expr { expr: None, .. } => {}
            ast::JsxChild::Expr {
                expr: Some(e),
                span,
            } if self.list_fold(p, pc, e, *span, t) => {}
            ast::JsxChild::Expr { span, .. } | ast::JsxChild::Spread { span, .. } => {
                let h = self.child_value(p, c, p.child);
                self.dynamic_child(p, pc, h, *span, t);
            }
            ast::JsxChild::Element(inner) => {
                let tag = inner.name.as_ref().and_then(intrinsic_tag);
                match tag {
                    Some(tag) if precompilable(p, inner, &tag) => {
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
    pub(super) fn dynamic_child(
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
        if h.ty == self.cx.ty.i64 || h.ty == self.cx.ty.f64 {
            // A number is written as `${n}`, as every provider renders it (no `jsxEscape`).
            if t.sep.is_some() {
                return t.run.push(TextPart::Number(h));
            }
            let s = self.number_text(h);
            return self.add_part(t, s);
        }
        let (textness, may_be_empty) = match t.sep {
            Some(_) => (self.textness(&h), self.may_be_empty(&h)),
            None => (Textness::Always, false),
        };
        // With `jsxEscapeString`, a `string` stays one until it is escaped (`escape_call`);
        // without it, it is a `Text` like any other text, or a slot if `Text` has no strings.
        let text = if h.ty == self.cx.ty.str_ && pc.escape_string.is_some() {
            Ok(h)
        } else {
            self.try_coerce(h, pc.text)
        };
        match text {
            Ok(h) if t.sep.is_some() => t.run.push(TextPart::Value(h, textness, may_be_empty)),
            Ok(h) => {
                let s = self.escape_call(p, pc, h, span);
                self.add_part(t, s);
            }
            Err(h) => {
                let frag = self.fragment_of(p, h, span);
                self.add_slot(p, pc, t, frag);
            }
        }
    }

    /// `Fragment([h], null)`.
    pub(super) fn fragment_of(&mut self, p: &Provider, h: hir::Expr, span: Span) -> hir::Expr {
        let child = self.as_child(p, h);
        let ty = self.cx.ty.array(p.child);
        let children = self.mk(H::ArrayLit(vec![child]), ty, span);
        let opt_str = self.cx.ty.option(self.cx.ty.str_);
        let key = self.mk(H::Lit(hir::Lit::Null), opt_str, span);
        self.jsx_call(p, p.fragment, "Fragment", vec![children, key], span)
    }

    /// A `string`-typed part of the current template string.
    pub(super) fn add_part(&mut self, t: &mut Template, h: hir::Expr) {
        self.flush_text(t, h.span);
        t.parts.push(h);
    }

    /// An `Element` slot: ends the current string.
    pub(super) fn add_slot(
        &mut self,
        p: &Provider,
        pc: Precompile,
        t: &mut Template,
        h: hir::Expr,
    ) {
        let span = h.span;
        self.end_run(p, pc, t, span, true);
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
        self.end_run(p, pc, t, span, false);
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

    /// `${h}` for a number `h`: digits, nothing to escape.
    pub(super) fn number_text(&mut self, h: hir::Expr) -> hir::Expr {
        let span = h.span;
        let str_ = self.cx.ty.str_;
        self.intrinsic(hir::Intrinsic::ToString, vec![h], str_, span)
    }

    /// `jsxEscape(h)` for a `JSX.Text` (or a `string`) `h`, or `jsxEscapeString(h)` for a
    /// `string` when the provider exports it: the same text, without converting the string to
    /// the `Text` union and testing which member it holds.
    pub(super) fn escape_call(
        &mut self,
        p: &Provider,
        pc: Precompile,
        h: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        if h.ty == self.cx.ty.str_ {
            if let Some(f) = pc.escape_string {
                let mut h = h;
                let moves_out = match &h.kind {
                    H::Field { mode, .. } | H::Index { mode, .. } => *mode == UseMode::Move,
                    _ => false,
                };
                if moves_out {
                    // `{f.message}`: borrowed where it is rather than copied out of `f`; the
                    // ownership pass makes it a move again if the parameter takes ownership.
                    set_place_mode(&mut h, UseMode::Borrow);
                }
                return self.jsx_call(p, f, "jsxEscapeString", vec![h], span);
            }
        }
        let h = self.coerce(h, pc.text);
        self.jsx_call(p, pc.escape, "jsxEscape", vec![h], span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn void_elements() {
        assert!(is_html_void("br"));
        assert!(is_html_void("img"));
        assert!(!is_html_void("div"));
        assert!(!is_html_void("my-br"));
    }
}
