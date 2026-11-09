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
//! - reads a property descriptor, whose `value` is such a read;
//! - makes a proxy whose handler isn't an object literal written directly as the second argument of
//!   `new Proxy(...)`, or whose literal has an `ownKeys` member, a computed key or a spread. Std
//!   reads the keys such a trap lists (`Object.values`, `entries`, a spread) with its own key, so a
//!   program can choose them; without one, std reads only the real own keys of a target, whose
//!   values are what the program stored there.
//!
//! The one exception is a forwarding `get` trap in such a literal: nothing else can reach it, so it
//! runs only with the key that was read, which the rules above account for. In a `get` that doesn't
//! use `this`, and whose key parameter has no default, `Reflect.get(t, k, ...)` and `t[k]` don't
//! count, with `t` and `k` its first two parameters, if nothing assigns either name
//! (`body::assigned`: destructuring targets, closures and their defaults included).
//!
//! Any other program has every algorithm compiled.

use std::collections::BTreeSet;

use rustc_hash::FxHashSet;
use velt_common::Span;
use velt_syntax::ast;

use super::literal_this::takes_this;
use crate::body::assigned::{assigned_in_expr, assigned_in_stmt, pattern_defaults, Assigned};
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
#[derive(Clone, Default)]
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

    /// The read at `span` of `target[key]`: a forwarding read, when they are the first two
    /// parameters of the trap being checked (`note_computed_read` counts the read itself).
    pub(crate) fn note_forward_key(&mut self, target: &ast::Expr, key: &ast::Expr, span: Span) {
        let Some((t, k)) = self.f.trap_key else {
            return;
        };
        if self.names_param(target, t) && self.names_param(key, k) {
            self.f.forward_reads.push(span);
        }
    }

    /// The read at `span` of `target[key]`, as `note_forward_key` with a checked `target`.
    pub(crate) fn note_index_forward(
        &mut self,
        target: &crate::hir::Expr,
        key: &ast::Expr,
        span: Span,
    ) {
        let (Some((t, k)), crate::hir::ExprKind::Local(l, ..)) = (self.f.trap_key, &target.kind)
        else {
            return;
        };
        if self.f.locals[l.0 as usize].span == t && self.names_param(key, k) {
            self.f.forward_reads.push(span);
        }
    }

    fn names_param(&self, e: &ast::Expr, param: Span) -> bool {
        let ast::ExprKind::Ident(id) = &e.kind else {
            return false;
        };
        let local = self.declared_local(&id.name);
        local.is_some_and(|l| self.f.locals[l.0 as usize].span == param)
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
                if let (Some(t), Some(k), false) = (args.first(), args.get(1), spread) {
                    self.note_forward_key(t, k, span);
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
            self.cx.method_facts.named = true;
            return;
        };
        for p in props {
            match p {
                // `[K]: ...` may be `ownKeys`, under a name not known here.
                ast::ObjectProp::KeyValue(name, _)
                    if name.name == "ownKeys" || name.name.starts_with('[') =>
                {
                    self.cx.method_facts.named = true;
                }
                ast::ObjectProp::KeyValue(name, value) => {
                    if name.name == "get" && matches!(value.kind, ast::ExprKind::Arrow { .. }) {
                        self.cx.method_facts.traps.insert(value.span);
                    }
                }
                ast::ObjectProp::Shorthand(name) => {
                    self.cx.method_facts.named |= name.name == "ownKeys";
                }
                ast::ObjectProp::Spread(_) => self.cx.method_facts.named = true,
                ast::ObjectProp::Method(m) => {
                    self.cx.method_facts.named |= m.sig.name.name == "ownKeys";
                }
            }
        }
    }

    /// A class made through a value (`new P(...)`), which may be `Proxy` with any handler.
    pub(crate) fn note_new_through_value(&mut self) {
        if self.in_user_module() {
            self.cx.method_facts.named = true;
        }
    }

    /// The closure `e` is being checked: the spans of its target and key parameters when it is a
    /// `get` trap that can only be called with the key that was read (module docs).
    pub(crate) fn trap_key_of(&self, e: &ast::Expr) -> Option<(Span, Span)> {
        let ast::ExprKind::Arrow { params, body, .. } = &e.kind else {
            return None;
        };
        if !self.cx.method_facts.traps.contains(&e.span) || takes_this(e) {
            return None;
        }
        let (t, k) = (params.first()?, params.get(1)?);
        for p in [t, k] {
            if p.pattern.is_some() || p.default.is_some() || p.optional || p.rest {
                return None;
            }
        }
        let mut written = Assigned::default();
        let mut defaults: Vec<&ast::Expr> =
            params.iter().filter_map(|p| p.default.as_ref()).collect();
        for p in params.iter().filter_map(|p| p.pattern.as_ref()) {
            pattern_defaults(p, &mut defaults);
        }
        for d in defaults {
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
        let free = |p: &ast::ArrowParam| !written.contains_key(p.name.name.as_str());
        (free(t) && free(k)).then_some((t.name.span, k.name.span))
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
