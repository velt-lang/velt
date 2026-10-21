//! The global names of a JavaScript environment, as `typeof` sees them and as values (#922). A
//! program written for both the browser and Node tests for its environment with
//! `typeof window === "undefined"`; Velt runs where Node does, so:
//!
//! - `typeof X` of a global Velt has no value for is the constant Node 22 gives: `"undefined"`
//!   for the browser's globals (`window`, `document`, `HTMLElement`, ...), `"object"` or
//!   `"function"` for Node's own (`globalThis`, `process`, `navigator`, `Event`, ...). A class's
//!   name is `"function"`.
//! - Reading a browser global throws the `ReferenceError` Node throws (`window is not defined`),
//!   so a branch that only runs in the browser still compiles, and behaves as on Node if it runs.
//!
//! A name the program declares is its own (a local, function, class or import), as in
//! JavaScript; these rules apply only to names it doesn't declare.

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::body::FnCx;
use crate::ctx::Item;
use crate::hir::{self, AdtKind};

/// The browser's globals (TypeScript's `lib.dom`) that Node 22 doesn't have: `typeof` is
/// `"undefined"`, and reading one throws a `ReferenceError`.
const BROWSER_ONLY: [&str; 23] = [
    "window",
    "document",
    "location",
    "self",
    "history",
    "screen",
    "localStorage",
    "sessionStorage",
    "customElements",
    "requestAnimationFrame",
    "cancelAnimationFrame",
    "getComputedStyle",
    "alert",
    "HTMLElement",
    "Element",
    "Node",
    "Text",
    "Comment",
    "DocumentFragment",
    "MutationObserver",
    "IntersectionObserver",
    "ResizeObserver",
    "XMLHttpRequest",
];

/// Node 22's own globals and what `typeof` gives for them, for names Velt may not have as values
/// (`typeof process`, `typeof navigator`).
pub(crate) const NODE_GLOBALS: [(&str, &str); 36] = [
    ("globalThis", "object"),
    ("process", "object"),
    ("console", "object"),
    ("Math", "object"),
    ("JSON", "object"),
    ("Reflect", "object"),
    ("navigator", "object"),
    ("performance", "object"),
    ("crypto", "object"),
    ("Object", "function"),
    ("Array", "function"),
    ("String", "function"),
    ("Number", "function"),
    ("Boolean", "function"),
    ("Symbol", "function"),
    ("Promise", "function"),
    ("Date", "function"),
    ("Map", "function"),
    ("Set", "function"),
    ("WeakMap", "function"),
    ("WeakSet", "function"),
    ("Error", "function"),
    ("RegExp", "function"),
    ("URL", "function"),
    ("URLSearchParams", "function"),
    ("Buffer", "function"),
    ("TextEncoder", "function"),
    ("TextDecoder", "function"),
    ("Event", "function"),
    ("EventTarget", "function"),
    ("CustomEvent", "function"),
    ("fetch", "function"),
    ("structuredClone", "function"),
    ("queueMicrotask", "function"),
    ("setTimeout", "function"),
    ("Proxy", "function"),
];

impl FnCx<'_, '_> {
    /// What `typeof name` is while compiling, for a name the program doesn't declare as a
    /// variable (`None`: an ordinary `typeof`, or an unknown name).
    pub(crate) fn global_typeof(&mut self, operand: &ast::Expr) -> Option<&'static str> {
        let ast::ExprKind::Ident(id) = &strip_parens(operand).kind else {
            return None;
        };
        if self.is_local_name(&id.name) {
            return None;
        }
        let node = NODE_GLOBALS
            .iter()
            .find(|(n, _)| *n == id.name)
            .map(|(_, t)| *t);
        match self.lookup_item(&id.name, id.span) {
            Some(Item::Def(d)) => {
                let a = self.cx.adt(d)?;
                // The standard library's version of a Node global (`Math` is a class here) is
                // what Node says; a class of the program's own is a constructor function.
                match self.cx.scopes[a.module].is_std {
                    true => node,
                    false => (a.kind == AdtKind::Class).then_some("function"),
                }
            }
            Some(_) => None,
            None if BROWSER_ONLY.contains(&id.name.as_str()) => Some("undefined"),
            None => node,
        }
    }

    /// Reading a browser global the program doesn't declare: Node's `ReferenceError` (`None`:
    /// `id` is not one).
    pub(crate) fn browser_global_read(&mut self, id: &ast::Ident) -> Option<hir::Expr> {
        if !BROWSER_ONLY.contains(&id.name.as_str()) || self.is_local_name(&id.name) {
            return None;
        }
        if self.lookup_item(&id.name, id.span).is_some() {
            return None;
        }
        let span = id.span;
        let error = synth(
            ast::ExprKind::New {
                class: ast::TypeExpr {
                    kind: ast::TypeExprKind::Named {
                        path: vec![ast::Ident {
                            name: "ReferenceError".to_string(),
                            span,
                        }],
                        args: vec![],
                    },
                    span,
                },
                args: vec![synth(
                    ast::ExprKind::Lit(ast::Lit::Str(format!("{} is not defined", id.name))),
                    span,
                )],
            },
            span,
        );
        Some(self.throw_expr(&error, span))
    }

    /// `typeof name` as the constant [`Self::global_typeof`] gives, as a string (`None`: not
    /// such a name).
    pub(crate) fn global_typeof_value(
        &mut self,
        operand: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        let tag = self.global_typeof(operand)?;
        Some(self.str_lit(tag, span))
    }

    /// `typeof name === tag` (`negate`: `!==`) for such a name: a constant.
    pub(crate) fn global_typeof_test(
        &mut self,
        operand: &ast::Expr,
        tag: &str,
        negate: bool,
        span: Span,
    ) -> Option<hir::Expr> {
        let found = self.global_typeof(operand)?;
        let b = self.cx.ty.bool_;
        Some(self.mk(
            hir::ExprKind::Lit(hir::Lit::Bool((found == tag) != negate)),
            b,
            span,
        ))
    }
}

/// `e` without parentheses.
fn strip_parens(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(inner) => strip_parens(inner),
        _ => e,
    }
}
