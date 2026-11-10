//! The global names of a JavaScript environment, as `typeof` sees them and as values (#922). A
//! program written for both the browser and Node tests for its environment with
//! `typeof window === "undefined"`; Velt runs where Node does, so:
//!
//! - `typeof X` of a global Velt has no value for is the constant Node 22 gives: `"undefined"`
//!   for other environments' globals (`window`, `document`, `Deno`, `require`, ...), `"object"` or
//!   `"function"` for Node's own (`globalThis`, `process`, `navigator`, `Event`, ...). A class's
//!   name is `"function"`. A global the prelude has only as a type is such a global too
//!   (`HTMLElement`).
//! - Reading a browser global throws the `ReferenceError` Node throws (`window is not defined`),
//!   so a branch that only runs in the browser still compiles, and behaves as on Node if it runs.
//!
//! A name the program declares is its own (a local, function, class or import), as in
//! JavaScript; these rules apply only to names it doesn't declare.

use velt_common::Span;
use velt_syntax::ast;

use super::dom_values::DOM_VALUES;
use super::setters::synth;
use crate::body::FnCx;
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir::{self, AdtKind};
use crate::type_meanings::Meaning;

/// The globals of other environments that Node 22 doesn't have in an ES module: the browser's
/// (TypeScript's `lib.dom`), other runtimes' (`Deno`, `Bun`) and CommonJS's (`require`,
/// `__dirname`). `typeof` is `"undefined"`, and reading one throws a `ReferenceError`.
const ABSENT: [&str; 29] = [
    "require",
    "module",
    "exports",
    "__dirname",
    "__filename",
    "Deno",
    "Bun",
    "window",
    "document",
    "location",
    "self",
    "history",
    "screen",
    "localStorage",
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
pub(crate) const NODE_GLOBALS: [(&str, &str); 54] = [
    ("globalThis", "object"),
    ("global", "object"),
    ("process", "object"),
    ("console", "object"),
    ("Math", "object"),
    ("JSON", "object"),
    ("Reflect", "object"),
    ("navigator", "object"),
    ("sessionStorage", "object"),
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
    ("Intl", "object"),
    ("BigInt", "function"),
    ("AbortController", "function"),
    ("AbortSignal", "function"),
    ("Response", "function"),
    ("Request", "function"),
    ("Headers", "function"),
    ("FormData", "function"),
    ("Blob", "function"),
    ("WebSocket", "function"),
    ("atob", "function"),
    ("btoa", "function"),
    ("setInterval", "function"),
    ("clearTimeout", "function"),
    ("setImmediate", "function"),
    ("SharedArrayBuffer", "function"),
];

/// Node's functions that the prelude has only as a type, and that aren't in [`NODE_GLOBALS`]
/// because `globalThis.Iterator` is still a slot of the program's. Any other such type has no
/// value in Node (`tests/golden/std/typeof_prelude_types` compares them).
const NODE_FUNCTIONS_TYPED: [&str; 2] = ["Function", "Iterator"];

impl FnCx<'_, '_> {
    /// `e instanceof SharedArrayBuffer` where the program declares no `SharedArrayBuffer`:
    /// Node has the class, but no Velt value is a shared buffer, so `false` (after `e`).
    pub(crate) fn shared_buffer_test(
        &mut self,
        e: &ast::Expr,
        ty: &ast::TypeExpr,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::TypeExprKind::Named { path, args } = &ty.kind else {
            return None;
        };
        let ([p], true) = (path.as_slice(), args.is_empty()) else {
            return None;
        };
        if p.name != "SharedArrayBuffer"
            || self.is_local_name(&p.name)
            || self
                .cx
                .lookup_item_at(self.module, &p.name, p.span)
                .is_some()
        {
            return None;
        }
        let s = self.expr(e, None, crate::body::Want::Borrow);
        let no = self.mk(
            hir::ExprKind::Lit(hir::Lit::Bool(false)),
            self.cx.ty.bool_,
            span,
        );
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(s),
                span,
            }],
            value: Some(Box::new(no)),
            span,
        };
        Some(self.mk(hir::ExprKind::Block(block), self.cx.ty.bool_, span))
    }

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
        let item = self.lookup_item(&id.name, id.span);
        if item.is_some_and(|it| self.prelude_type_only(&id.name, it)) {
            return Some(match node {
                Some(tag) => tag,
                None if NODE_FUNCTIONS_TYPED.contains(&id.name.as_str()) => "function",
                None => "undefined",
            });
        }
        // The program's own type next to Node's value: that value, whether or not a module has
        // named the prelude's type too (`Ctx::global_value`).
        if node.is_some()
            && item.is_some_and(|it| self.cx.meaning(it) == Meaning::Type)
            && (self.cx.prelude.get(id.name.as_str()))
                .is_some_and(|p| self.cx.meaning(*p) == Meaning::Type)
        {
            return node;
        }
        match item {
            // A function, generic or not.
            Some(Item::Def(d)) if matches!(self.cx.info[d.0 as usize], DefInfo::Fn(_)) => {
                Some("function")
            }
            // The standard library's interface for a Node global (`AbortController`).
            Some(Item::Def(d)) if self.cx.adt(d).is_none() => {
                let m = self.cx.iface(d)?.module;
                self.cx.scopes[m].is_std.then_some(node).flatten()
            }
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
            None if ABSENT.contains(&id.name.as_str()) => Some("undefined"),
            None => node,
        }
    }

    /// Whether `name`, found as `item`, is a global the prelude has only as a type (`HTMLElement`,
    /// `Iterable`): no name of this module, so there is no variable behind it but Node's. A type
    /// the module declares or imports is its own: `typeof` of that is refused, as under `tsc`.
    pub(super) fn prelude_type_only(&self, name: &str, item: Item) -> bool {
        let only_a_type = match item {
            // A struct has no value in Velt (`RequestInit`, an interface under `tsc`).
            Item::Def(d) if self.cx.adt(d).is_some_and(|a| a.kind == AdtKind::Struct) => true,
            // A class of std's lib.dom subset (`KeyboardEvent`, a class so that it converts to
            // `Event`): an interface under `tsc`, with no value in Node.
            Item::Def(d) if self.cx.std_dom_class(d) => true,
            _ => self.cx.meaning(item) == Meaning::Type,
        };
        only_a_type
            && self.cx.prelude.get(name) == Some(&item)
            && !self.cx.scopes[self.module].items.contains_key(name)
    }

    /// Is `name` another environment's global the program doesn't declare: one of [`ABSENT`],
    /// or a value of `lib.dom` that Node doesn't have and whose type the prelude has
    /// (`HTMLElement`, `Node`: `DOM_VALUES`)?
    pub(crate) fn browser_global(&mut self, name: &str, span: Span) -> bool {
        let dom = DOM_VALUES.binary_search(&name).is_ok()
            && !NODE_GLOBALS.iter().any(|(n, _)| *n == name);
        if !dom && !ABSENT.contains(&name) || self.is_local_name(name) {
            return false;
        }
        match self.lookup_item(name, span) {
            None => true,
            Some(it) => self.prelude_type_only(name, it),
        }
    }

    /// Reading such a global the program doesn't declare: Node's `ReferenceError` (`None`:
    /// `id` is not one).
    pub(crate) fn browser_global_read(&mut self, id: &ast::Ident) -> Option<hir::Expr> {
        if !self.browser_global(&id.name, id.span) {
            return None;
        }
        Some(self.not_defined(&id.name, id.span))
    }

    /// `throw new ReferenceError("<name> is not defined")`, as Node reads an undeclared name.
    pub(crate) fn not_defined(&mut self, name: &str, span: Span) -> hir::Expr {
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
                    ast::ExprKind::Lit(ast::Lit::Str(format!("{name} is not defined"))),
                    span,
                )],
            },
            span,
        );
        self.throw_expr(&error, span)
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
