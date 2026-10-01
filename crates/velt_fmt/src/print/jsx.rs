//! JSX elements, prettier style: attributes on one line or one per line (`>` / `/>` on its own
//! line), children laid out by [`super::jsx_children`] between the tags (indented, on their own
//! lines when the element breaks), and multi-line elements wrapped in parentheses after
//! `return`, `=`, `=>`, `&&`/`||`/`??` and in ternary branches.
//!
//! The AST is never changed: parentheses around an element are not part of it (the parser drops
//! them), self-closing tags stay self-closing, attribute strings keep their entities, and no
//! `{" "}` is introduced (a space that matters stays on its line instead).

use velt_syntax::ast::{
    BinaryOp, Expr, ExprKind, JsxAttr, JsxAttrValue, JsxChild, JsxElement, JsxName,
};

use super::func::breaks_itself;
use super::jsx_children::Edge;
use super::Printer;
use crate::doc::{
    break_parent, cat, concat, fill, group, hardline, if_break, indent, join, line, nil, softline,
    text, Doc,
};
use crate::source::slice;

impl Printer<'_> {
    /// `e`, wrapped in parentheses when it is a JSX element that spans several lines.
    pub(super) fn expr_jsx_parens(&mut self, e: &Expr) -> Doc {
        let ExprKind::Jsx(el) = &e.kind else {
            return self.expr(e);
        };
        self.with_leading(e.span.lo, |p| {
            let inner = p.jsx_element(el);
            group(cat![
                if_break("(", ""),
                indent(cat![softline(), inner]),
                softline(),
                if_break(")", "")
            ])
        })
    }

    /// An element or fragment.
    pub(super) fn jsx_element(&mut self, el: &JsxElement) -> Doc {
        let name = el.name.as_ref().map(JsxName::to_source).unwrap_or_default();
        let open_end = el.children.first().map_or(el.span.hi, child_lo);
        let self_closing = el.children.is_empty() && is_self_closing(slice(self.src, el.span));
        let open = self.jsx_opening(&name, &el.attrs, open_end, self_closing);
        if self_closing {
            return open;
        }
        let children = self.jsx_children(&el.children);
        let close = self.jsx_closing(&name, el.span.hi);
        if children.parts.is_empty() {
            return cat![open, children.start.glue(), close];
        }
        let forced = forces_break(el);
        let edge = |e: Edge| match e {
            Edge::Glue(s) => text(s),
            Edge::Break if forced => hardline(),
            Edge::Break => softline(),
        };
        group(cat![
            open,
            indent(cat![edge(children.start), fill(children.parts)]),
            edge(children.end),
            close
        ])
    }

    /// `<name attrs>` / `<name attrs />`; comments up to `end` stay inside the tag.
    fn jsx_opening(&mut self, name: &str, attrs: &[JsxAttr], end: u32, self_closing: bool) -> Doc {
        let mut docs: Vec<Doc> = attrs
            .iter()
            .map(|a| self.with_leading(attr_lo(a), |p| p.jsx_attr(a)))
            .collect();
        docs.extend(self.comments.take_before(end).into_iter().map(|c| {
            let after = if c.needs_newline() {
                break_parent()
            } else {
                nil()
            };
            cat![text(c.text), after]
        }));
        if docs.is_empty() {
            let tail = if self_closing { " />" } else { ">" };
            return text(format!("<{name}{tail}"));
        }
        let tail = if self_closing {
            cat![line(), "/>"]
        } else {
            cat![softline(), ">"]
        };
        group(cat![
            "<",
            name.to_string(),
            indent(cat![line(), join(&line(), docs)]),
            tail
        ])
    }

    /// `</name>`, with any comments left before `hi` inside it.
    fn jsx_closing(&mut self, name: &str, hi: u32) -> Doc {
        let comments = self.comments.take_before(hi);
        let docs = comments.into_iter().map(|c| {
            let after = if c.needs_newline() { hardline() } else { nil() };
            cat![" ", text(c.text), after]
        });
        cat!["</", name.to_string(), concat(docs.collect()), ">"]
    }

    fn jsx_attr(&mut self, attr: &JsxAttr) -> Doc {
        match attr {
            JsxAttr::Spread { expr, span } => self.jsx_spread(expr, span.hi),
            JsxAttr::Named { name, value, .. } => {
                let value = match value {
                    None => nil(),
                    Some(JsxAttrValue::Str { span, .. }) => {
                        cat!["=", attr_string(slice(self.src, *span))]
                    }
                    Some(JsxAttrValue::Expr { expr, span }) => {
                        cat!["=", self.jsx_container(expr, span.hi)]
                    }
                    Some(JsxAttrValue::Element(el)) => cat!["=", self.jsx_element(el)],
                };
                cat![name.to_source(), value]
            }
        }
    }

    /// `{expr}` ending at `hi`: hugged when the expression breaks well by itself, else the
    /// expression moves inside indented braces when too long.
    pub(super) fn jsx_container(&mut self, expr: &Expr, hi: u32) -> Doc {
        let inner = self.expr(expr);
        let dangling = self.jsx_dangling(hi);
        if hugs(expr) {
            return group(cat!["{", inner, dangling, "}"]);
        }
        group(cat![
            "{",
            indent(cat![softline(), inner, dangling]),
            softline(),
            "}"
        ])
    }

    /// `{...expr}` ending at `hi`.
    pub(super) fn jsx_spread(&mut self, expr: &Expr, hi: u32) -> Doc {
        let inner = self.expr(expr);
        cat!["{...", inner, self.jsx_dangling(hi), "}"]
    }

    /// Comments before `hi` after the last printed node inside braces (`{x /* c */}`).
    pub(super) fn jsx_dangling(&mut self, hi: u32) -> Doc {
        let parts = self.comments.take_before(hi).into_iter().map(|c| {
            let after = if c.needs_newline() { hardline() } else { nil() };
            cat![" ", text(c.text), after]
        });
        concat(parts.collect())
    }

    /// `cond ? a : b` where a branch is a JSX element: `cond ? (` … `) : (` … `)`.
    pub(super) fn jsx_conditional(&mut self, cond: &Expr, then: &Expr, els: &Expr) -> Doc {
        let cond = self.expr(cond);
        let then = self.expr_jsx_parens(then);
        let els = self.expr_jsx_parens(els);
        group(cat![cond, " ? ", then, " : ", els])
    }
}

/// Is `e` laid out around a JSX element (an element, `a && <b />`, `c ? <a /> : b`), so it can
/// start on the line of the `=`, `=>` or `return` in front of it?
pub(super) fn is_jsx_layout(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Jsx(_) => true,
        ExprKind::Cond { then, els, .. } => is_jsx_conditional(then, els),
        ExprKind::Binary { op, rhs, .. } => is_jsx_operand(*op, rhs),
        _ => false,
    }
}

/// Does a conditional need the JSX layout (a branch is an element)?
pub(super) fn is_jsx_conditional(then: &Expr, els: &Expr) -> bool {
    matches!(then.kind, ExprKind::Jsx(_)) || matches!(els.kind, ExprKind::Jsx(_))
}

/// `a && <b />`: the element follows the operator on its line (wrapped in parentheses when it
/// breaks).
pub(super) fn is_jsx_operand(op: BinaryOp, rhs: &Expr) -> bool {
    matches!(op, BinaryOp::And | BinaryOp::Or | BinaryOp::Nullish)
        && matches!(rhs.kind, ExprKind::Jsx(_))
}

/// Prettier's rule: an element whose children include an element, several expression
/// containers, or that has several attributes, always puts its children on their own lines.
fn forces_break(el: &JsxElement) -> bool {
    let containers = el
        .children
        .iter()
        .filter(|c| matches!(c, JsxChild::Expr { .. } | JsxChild::Spread { .. }))
        .count();
    let has_element = el
        .children
        .iter()
        .any(|c| matches!(c, JsxChild::Element(_)));
    has_element || containers > 1 || (el.name.is_some() && el.attrs.len() > 1)
}

/// Expressions printed directly inside `{}` (they break well by themselves).
fn hugs(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Jsx(_) | ExprKind::Cond { .. } | ExprKind::Binary { .. } => true,
        _ => breaks_itself(e),
    }
}

/// An attribute string in double quotes, unless it contains `"` (no escapes exist in JSX).
fn attr_string(raw: &str) -> String {
    let inner = raw.get(1..raw.len().saturating_sub(1)).unwrap_or("");
    if raw.starts_with('\'') && !inner.contains('"') {
        format!("\"{inner}\"")
    } else {
        raw.to_string()
    }
}

/// Was the element written `<a />` (rather than `<a></a>`)? Its source then ends with the
/// `/>` token, not with the `*/>` of a comment in a closing tag.
fn is_self_closing(source: &str) -> bool {
    source.ends_with("/>") && !source.ends_with("*/>")
}

fn attr_lo(attr: &JsxAttr) -> u32 {
    match attr {
        JsxAttr::Spread { span, .. } | JsxAttr::Named { span, .. } => span.lo,
    }
}

/// Where a child starts in the source.
fn child_lo(child: &JsxChild) -> u32 {
    match child {
        JsxChild::Text { span, .. }
        | JsxChild::Expr { span, .. }
        | JsxChild::Spread { span, .. } => span.lo,
        JsxChild::Element(el) => el.span.lo,
    }
}
