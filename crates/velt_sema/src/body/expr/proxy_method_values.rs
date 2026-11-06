//! Can a program hold an array method under another name (#2424; `Program::array_methods_open`)?
//! An array method's algorithm (velt/lang/proxy/array) is compiled only for the names the program
//! calls through `any`. That is enough while a method read from a proxy or an array is only ever
//! reached by its own name. An `ArrayMethod` value comes from one kind of operation: a read through
//! `any` or `Reflect.get` whose key names the method. So the names called cover what can run unless
//! the program's own code
//!
//! - reads a member through `any` or `Reflect.get` by a computed key, or by a literal key naming an
//!   array method or `get`;
//! - calls a member `get` through `any`, which may be `Reflect.get` held as a value;
//! - reads a property descriptor, whose `value` is such a read.
//!
//! The one exception is a forwarding `get` trap: an object literal written directly as the second
//! argument of `new Proxy(...)`, nothing else can reach it, so its `get` runs only with the key that
//! was read, which the rules above account for. In such a `get`, which doesn't use `this` and whose
//! key parameter has no default, a read by that parameter doesn't count, if nothing assigns the
//! name (`body::assigned`: destructuring targets, closures and their defaults included).
//!
//! Any other program has every algorithm compiled.

use std::collections::BTreeSet;

use rustc_hash::FxHashSet;
use velt_common::Span;
use velt_syntax::ast;

use super::literal_this::takes_this;
use crate::body::assigned::{assigned_in_expr, assigned_in_stmt, Assigned};
use crate::body::frame::Frame;
use crate::body::FnCx;
use crate::ctx::{Ctx, Item};

/// The methods a proxy of an array runs generically (std/lang/proxy/array_names.velt).
pub(crate) const ARRAY_METHODS: [&str; 18] = [
    "push",
    "pop",
    "shift",
    "unshift",
    "splice",
    "map",
    "filter",
    "forEach",
    "some",
    "every",
    "find",
    "includes",
    "indexOf",
    "lastIndexOf",
    "join",
    "slice",
    "reverse",
    "sort",
];

/// What sema saw while checking bodies.
#[derive(Default)]
pub(crate) struct MethodFacts {
    /// The program's reads by a computed key.
    reads: Vec<Span>,
    /// The ones a forwarding trap makes with its own key.
    forwards: FxHashSet<Span>,
    /// One of its reads has a literal key naming an array method or `get`.
    named: bool,
    /// The `get` members of handlers written directly in `new Proxy(...)`.
    traps: FxHashSet<Span>,
    /// The literal names its own code calls through `any` (`Program::any_call_names`).
    pub(crate) call_names: BTreeSet<String>,
}

fn names_a_method(name: &str) -> bool {
    name == "get" || ARRAY_METHODS.contains(&name)
}

impl FnCx<'_, '_> {
    fn in_user_module(&self) -> bool {
        !self.cx.scopes[self.module].is_std
    }

    /// A read through `any` with the literal key `name`, in the program's own code.
    pub(crate) fn note_named_read(&mut self, name: &str) {
        if self.in_user_module() && names_a_method(name) {
            self.cx.method_facts.named = true;
        }
    }

    /// A read through `any` by a computed key, at `span`.
    pub(crate) fn note_computed_read(&mut self, span: Span) {
        if self.in_user_module() {
            self.cx.method_facts.reads.push(span);
        }
    }

    /// The computed key `key` of the read at `span`: a forwarding read, when it is the key
    /// parameter of the trap being checked (`note_computed_read` counts the read itself).
    pub(crate) fn note_forward_key(&mut self, key: &ast::Expr, span: Span) {
        let (Some(param), ast::ExprKind::Ident(id)) = (self.f.trap_key, &key.kind) else {
            return;
        };
        let local = self.declared_local(&id.name);
        if local.is_some_and(|l| self.f.locals[l.0 as usize].span == param) {
            self.f.forward_reads.push(span);
        }
    }

    /// `Reflect.get(args)` at `span`.
    pub(crate) fn note_reflect_get(&mut self, args: &[ast::Expr], span: Span) {
        let spread = args
            .iter()
            .any(|a| matches!(a.kind, ast::ExprKind::Spread(_)));
        match args.get(1).map(|k| &k.kind) {
            Some(ast::ExprKind::Lit(ast::Lit::Str(k))) if !spread => self.note_named_read(k),
            _ => {
                self.note_computed_read(span);
                if let (Some(k), false) = (args.get(1), spread) {
                    self.note_forward_key(k, span);
                }
            }
        }
    }

    /// A property descriptor is read: its `value` is a read of that key.
    pub(crate) fn note_descriptor_read(&mut self) {
        if self.in_user_module() {
            self.cx.method_facts.named = true;
        }
    }

    /// `new class(args)`: a handler written directly in `new Proxy(...)` has its `get` noted.
    pub(crate) fn note_direct_handler(&mut self, class: &ast::TypeExpr, args: &[ast::Expr]) {
        let ast::TypeExprKind::Named { path, .. } = &class.kind else {
            return;
        };
        if !self.in_user_module() || path.len() != 1 || path[0].name != "Proxy" {
            return;
        }
        let proxy = self.cx.prelude_adt("Proxy");
        if !matches!(self.lookup_type_path(path), Some(Item::Def(d)) if Some(d) == proxy) {
            return;
        }
        let Some(ast::ExprKind::Object(props)) = args.get(1).map(|h| &h.kind) else {
            return;
        };
        for p in props {
            if let ast::ObjectProp::KeyValue(name, value) = p {
                if name.name == "get" && matches!(value.kind, ast::ExprKind::Arrow { .. }) {
                    self.cx.method_facts.traps.insert(value.span);
                }
            }
        }
    }

    /// The closure `e` is being checked: the span of its key parameter when it is a `get` trap
    /// that can only be called with the key that was read (module docs).
    pub(crate) fn trap_key_of(&self, e: &ast::Expr) -> Option<Span> {
        let ast::ExprKind::Arrow { params, body, .. } = &e.kind else {
            return None;
        };
        if !self.cx.method_facts.traps.contains(&e.span) || takes_this(e) {
            return None;
        }
        let k = params.get(1)?;
        if k.pattern.is_some() || k.default.is_some() || k.optional || k.rest {
            return None;
        }
        let mut written = Assigned::default();
        for d in params.iter().filter_map(|p| p.default.as_ref()) {
            assigned_in_expr(d, &mut written);
        }
        match body {
            ast::ArrowBody::Expr(b) => assigned_in_expr(b, &mut written),
            ast::ArrowBody::Block(b) => {
                for s in &b.stmts {
                    assigned_in_stmt(s, &mut written);
                }
            }
        }
        (!written.contains_key(k.name.name.as_str())).then_some(k.name.span)
    }

    /// The closure with `frame` has been checked: its forwarding reads count.
    pub(crate) fn finish_trap(&mut self, frame: &Frame) {
        self.cx
            .method_facts
            .forwards
            .extend(frame.forward_reads.iter().copied());
    }
}

/// `Program::array_methods_open` and `Program::any_call_names`.
pub(crate) fn finish(cx: &mut Ctx) -> (bool, Vec<String>) {
    let f = &cx.method_facts;
    let open = cx.prelude_adt("Proxy").is_some()
        && (f.named
            || f.call_names.contains("get")
            || f.reads.iter().any(|r| !f.forwards.contains(r)));
    let names = std::mem::take(&mut cx.method_facts.call_names);
    (open, names.into_iter().collect())
}
