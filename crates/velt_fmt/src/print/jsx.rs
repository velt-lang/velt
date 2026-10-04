//! JSX elements, prettier style: attributes on one line or one per line (`>` / `/>` on its own
//! line), children laid out by [`super::jsx_children`] and [`super::jsx_layout`] between the
//! tags (indented, on their own lines when the element breaks), multi-line elements wrapped in
//! parentheses after `return`, `=`, `=>` and `&&`/`||`/`??`, and conditionals with an element
//! in them in prettier's JSX mode (element branches wrapped in parentheses when the chain
//! breaks).
//!
//! What an element renders never changes: parentheses around an element are not part of the
//! AST (the parser drops them), self-closing tags stay self-closing, attribute strings keep their
//! entities, and text only moves where React's whitespace rules allow it. Like prettier, a space
//! next to a tag may turn into `{" "}` where the line breaks (and back): the AST then differs
//! only in how the same text is split between text and string children.

use velt_syntax::ast::{
    ArrowBody, BinaryOp, Expr, ExprKind, JsxAttr, JsxAttrValue, JsxChild, JsxElement, JsxName, Lit,
};

use super::func::breaks_itself;
use super::jsx_children::{child_bounds, is_meaningful, Child};
use super::jsx_layout::{element, Children};
use super::Printer;
use crate::doc::{
    break_parent, cat, concat, group, group_broken, hardline, if_break, indent, join, line, nil,
    softline, text, Doc,
};
use crate::source::slice;

impl Printer<'_> {
    /// `e`, wrapped in parentheses when it is a JSX element that spans several lines.
    pub(super) fn expr_jsx_parens(&mut self, e: &Expr) -> Doc {
        let ExprKind::Jsx(el) = &e.kind else {
            return self.expr(e);
        };
        let broken = self.broken_jsx_bodies.remove(&e.span.lo);
        self.with_leading(e.span.lo, |p| {
            let inner = p.jsx_element(el);
            let wrapped = cat![
                if_break("(", ""),
                indent(cat![softline(), inner]),
                softline(),
                if_break(")", "")
            ];
            if broken {
                group_broken(wrapped)
            } else {
                group(wrapped)
            }
        })
    }

    /// An element or fragment (prettier's `printJsxElementInternal`): on one line if it fits
    /// and nothing forces a break, else the children go on their own lines between the tags.
    pub(super) fn jsx_element(&mut self, el: &JsxElement) -> Doc {
        let name = el.name.as_ref().map(JsxName::to_source).unwrap_or_default();
        let open_end = el
            .children
            .first()
            .map_or(el.span.hi, |c| child_bounds(c).0);
        let self_closing = el.children.is_empty() && is_self_closing(slice(self.src, el.span));
        let tag = cat![name.clone(), self.type_args(&el.type_args)];
        let open = self.jsx_opening(tag, &el.attrs, open_end, self_closing);
        if self_closing {
            return open;
        }
        let exact = keeps_children(el);
        let children = self.jsx_virtual_children(el, exact);
        if let [Child::Node(JsxChild::Expr {
            expr: Some(e),
            span,
        })] = children.as_slice()
        {
            if matches!(e.kind, ExprKind::Template { .. }) {
                let child = self.jsx_container(e, span.hi);
                let close = self.jsx_closing(&name, el.span.hi);
                // Like prettier, a template over several lines breaks the parentheses around.
                let breaks = if self.has_multiline_template(&el.children) {
                    break_parent()
                } else {
                    nil()
                };
                return cat![open, child, close, breaks];
            }
        }
        let contains_text = children
            .iter()
            .any(|c| matches!(c, Child::Text(raw) if is_meaningful(raw)));
        let expressions = children
            .iter()
            .filter(|c| matches!(c, Child::Node(JsxChild::Expr { .. })))
            .count();
        let parts = self.jsx_parts(&children);
        let close = self.jsx_closing(&name, el.span.hi);
        let forced = open.breaks()
            || el
                .children
                .iter()
                .any(|c| matches!(c, JsxChild::Element(_)))
            || expressions > 1
            || (el.name.is_some() && el.attrs.len() > 1)
            || self.has_multiline_template(&el.children);
        let children = Children {
            parts,
            contains_text,
            exact,
        };
        element(open, close, children, forced)
    }

    /// Is a child a template literal written over several lines (prettier: its text forces the
    /// element to break)?
    fn has_multiline_template(&self, children: &[JsxChild]) -> bool {
        children.iter().any(|c| {
            matches!(c, JsxChild::Expr { expr: Some(e), span }
                if matches!(e.kind, ExprKind::Template { .. })
                    && slice(self.src, *span).contains('\n'))
        })
    }

    /// `<name attrs>` / `<name attrs />` (`tag`: the name with its type arguments); comments up
    /// to `end` stay inside the tag.
    fn jsx_opening(&mut self, tag: Doc, attrs: &[JsxAttr], end: u32, self_closing: bool) -> Doc {
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
            return cat!["<", tag, tail];
        }
        let tail = if self_closing {
            cat![line(), "/>"]
        } else {
            cat![softline(), ">"]
        };
        group(cat![
            "<",
            tag,
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
        if let ExprKind::Call { args, .. } = &expr.kind {
            let bodies = args.iter().filter_map(|a| match &a.kind {
                ExprKind::Arrow {
                    body: ArrowBody::Expr(body),
                    ..
                } if matches!(body.kind, ExprKind::Jsx(_)) => Some(body.span.lo),
                _ => None,
            });
            self.broken_jsx_bodies.extend(bodies);
        }
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

    /// A conditional chain with an element in it, in prettier's JSX mode: `cond ? (` … `) : (`
    /// … `)`, one group for the whole chain.
    pub(super) fn jsx_conditional(&mut self, cond: &Expr, then: &Expr, els: &Expr) -> Doc {
        group(self.jsx_conditional_chain(cond, then, els))
    }

    fn jsx_conditional_chain(&mut self, cond: &Expr, then: &Expr, els: &Expr) -> Doc {
        let cond = self.expr(cond);
        let then = self.jsx_branch(then, false);
        let els = self.jsx_branch(els, true);
        cat![cond, " ? ", then, " : ", els]
    }

    /// A branch in JSX mode. An element is wrapped in parentheses when the chain breaks;
    /// `null` and a conditional continuing the chain as the alternate are printed as they are.
    /// Prettier wraps any other branch too, but here parentheses are part of the AST (only those
    /// around an element are layout), so such a branch keeps the parentheses it was written with
    /// and moves to its own line only when it does not fit.
    fn jsx_branch(&mut self, e: &Expr, alternate: bool) -> Doc {
        match &e.kind {
            ExprKind::Lit(Lit::Null) => self.expr(e),
            ExprKind::Cond { cond, then, els } if alternate => {
                self.with_leading(e.span.lo, |p| p.jsx_conditional_chain(cond, then, els))
            }
            ExprKind::Jsx(_) => {
                let inner = self.with_leading(e.span.lo, |p| p.expr(e));
                cat![
                    if_break("(", ""),
                    indent(cat![softline(), inner]),
                    softline(),
                    if_break(")", "")
                ]
            }
            _ => {
                let inner = self.with_leading(e.span.lo, |p| p.expr(e));
                group(indent(cat![softline(), inner]))
            }
        }
    }
}

/// Is `e` laid out around a JSX element (an element, `a && <b />`, `c ? <a /> : b`), so it can
/// start on the line of the `=`, `=>` or `return` in front of it?
pub(super) fn is_jsx_layout(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Jsx(_) => true,
        ExprKind::Cond { cond, then, els } => is_jsx_conditional(cond, then, els),
        ExprKind::Binary { op, rhs, .. } => is_jsx_operand(*op, rhs),
        _ => false,
    }
}

/// Does a conditional need the JSX layout: is there an element among the operands of the
/// conditionals of its chain (prettier's `conditionalExpressionChainContainsJsx`)?
pub(super) fn is_jsx_conditional(cond: &Expr, then: &Expr, els: &Expr) -> bool {
    [cond, then, els].into_iter().any(|e| match &e.kind {
        ExprKind::Jsx(_) => true,
        ExprKind::Cond { cond, then, els } => is_jsx_conditional(cond, then, els),
        _ => false,
    })
}

/// `a && <b />`: the element follows the operator on its line (wrapped in parentheses when it
/// breaks).
pub(super) fn is_jsx_operand(op: BinaryOp, rhs: &Expr) -> bool {
    matches!(op, BinaryOp::And | BinaryOp::Or | BinaryOp::Nullish)
        && matches!(rhs.kind, ExprKind::Jsx(_))
}

/// Does `el` keep its children exactly as written? A component receives them as its `children`
/// prop (one child as itself, several as an array), so only intrinsic elements and fragments,
/// whose children are only rendered, may trade a space for `{" "}`.
fn keeps_children(el: &JsxElement) -> bool {
    match &el.name {
        // Sema's `intrinsic_tag`: a component unless lower-case or dashed (`_Card` is one).
        Some(JsxName::Ident(id)) => {
            !(id.name.starts_with(|c: char| c.is_ascii_lowercase()) || id.name.contains('-'))
        }
        Some(JsxName::Member(_)) => true,
        Some(JsxName::Namespaced(..)) | None => false,
    }
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
pub(super) fn is_self_closing(source: &str) -> bool {
    source.ends_with("/>") && !source.ends_with("*/>")
}

fn attr_lo(attr: &JsxAttr) -> u32 {
    match attr {
        JsxAttr::Spread { span, .. } | JsxAttr::Named { span, .. } => span.lo,
    }
}
