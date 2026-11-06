//! Can a program hold an array method under another name (#2424; `Program::array_methods_open`)?
//! An array method's algorithm (velt/lang/proxy/array) is compiled only for the names the program
//! calls through `any`. That is enough while a method read from a proxy or an array is only ever
//! reached by its own name. An `ArrayMethod` value comes from one kind of operation: a read through
//! `any` or `Reflect.get` whose key names the method. So the names called cover what can run unless
//! the program's own code
//!
//! - reads a member through `any` by a computed key, or by a literal key naming an array method;
//! - calls `Reflect.get` likewise, or uses `Reflect` as a value;
//! - reads a property descriptor, whose `value` is such a read;
//! - reads a member named `get` as a value, or calls one with two or more arguments: it may be a
//!   trap of the next rule, called with a key that isn't the one read.
//!
//! The exception is what a forwarding `get` trap does: inside an object literal's `get` member,
//! `Reflect.get(t, k, r)` and `t[k]` with `k` its key parameter, never assigned or shadowed there.
//! The proxy calls the trap with the key that was read, which the rules above already account for.
//!
//! Any other program has every algorithm compiled.

use std::collections::HashSet;

use velt_common::Span;
use velt_syntax::ast;
use velt_syntax::visit::{walk_block, walk_expr, walk_module, Visit};

use crate::ctx::Ctx;

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

fn is_ident(e: &ast::Expr, name: &str) -> bool {
    matches!(&e.kind, ast::ExprKind::Ident(i) if i.name == name)
}

/// `Reflect.get(...)`'s arguments, when `e` is such a call.
fn reflect_get(e: &ast::Expr) -> Option<&[ast::Expr]> {
    let ast::ExprKind::Call { callee, args, .. } = &e.kind else {
        return None;
    };
    let ast::ExprKind::Member { object, prop, .. } = &callee.kind else {
        return None;
    };
    (is_ident(object, "Reflect") && prop.name == "get").then_some(args.as_slice())
}

/// The forwarding reads in one trap's body, and whether its key parameter is rebound there.
struct TrapBody<'k> {
    key: &'k str,
    rebound: bool,
    forwards: Vec<Span>,
}

impl<'a> Visit<'a> for TrapBody<'_> {
    fn var_decl(&mut self, v: &'a ast::VarDecl) {
        self.binds(&v.pattern);
    }

    fn stmt(&mut self, s: &'a ast::Stmt) {
        if let ast::StmtKind::Try {
            catch: Some((Some(p), _)),
            ..
        } = &s.kind
        {
            self.binds(p);
        }
    }

    fn function(&mut self, sig: &'a ast::FnSig, _: &'a ast::Block) {
        self.rebound |= sig.params.iter().any(|p| p.name.name == self.key);
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        match &e.kind {
            ast::ExprKind::Assign { target, .. } | ast::ExprKind::Update { target, .. } => {
                self.rebound |= is_ident(target, self.key);
            }
            ast::ExprKind::Arrow { params, .. } => {
                self.rebound |= params.iter().any(|p| p.name.name == self.key);
            }
            ast::ExprKind::Index { index, more, .. } => {
                if more.is_empty() && is_ident(index, self.key) {
                    self.forwards.push(e.span);
                }
            }
            _ => {
                if reflect_get(e).is_some_and(|a| a.get(1).is_some_and(|k| is_ident(k, self.key))) {
                    self.forwards.push(e.span);
                }
            }
        }
    }
}

impl TrapBody<'_> {
    fn binds(&mut self, p: &ast::Pattern) {
        // Any name in the pattern, bound or not: conservative, and only inside a trap.
        self.rebound |= format!("{p:?}").contains(self.key);
    }
}

/// What the program's own modules do (module docs).
#[derive(Default)]
struct Scan {
    open: bool,
    /// Forwarding reads of `get` traps.
    forwards: HashSet<Span>,
    /// Calls of a member `get` with at most one argument (`map.get(k)`).
    get_calls: HashSet<Span>,
    /// Members named `get`.
    gets: Vec<Span>,
    /// `Reflect.get` calls other than with a literal key that names no array method.
    reflect_reads: Vec<Span>,
    /// `Reflect` as written, and those with a member read from them.
    reflect_uses: Vec<Span>,
    reflect_members: HashSet<Span>,
}

impl Scan {
    /// The `get` member of an object literal, with key parameter `key`: its forwarding reads.
    fn trap(&mut self, key: Option<&str>, body: &mut dyn FnMut(&mut TrapBody)) {
        let Some(key) = key else {
            return;
        };
        let mut t = TrapBody {
            key,
            rebound: false,
            forwards: vec![],
        };
        body(&mut t);
        if !t.rebound {
            self.forwards.extend(t.forwards);
        }
    }
}

impl<'a> Visit<'a> for Scan {
    fn expr(&mut self, e: &'a ast::Expr) {
        if is_ident(e, "Reflect") {
            self.reflect_uses.push(e.span);
        }
        if let Some(args) = reflect_get(e) {
            match args.get(1).map(|k| &k.kind) {
                Some(ast::ExprKind::Lit(ast::Lit::Str(k)))
                    if !ARRAY_METHODS.contains(&k.as_str()) => {}
                _ => self.reflect_reads.push(e.span),
            }
        }
        match &e.kind {
            ast::ExprKind::Call { callee, args, .. } => {
                if let ast::ExprKind::Member { prop, .. } = &callee.kind {
                    if prop.name == "get" && (args.len() <= 1 || reflect_get(e).is_some()) {
                        self.get_calls.insert(callee.span);
                    }
                }
            }
            ast::ExprKind::Member { object, prop, .. } => {
                if prop.name == "get" {
                    self.gets.push(e.span);
                }
                if prop.name == "getOwnPropertyDescriptor" {
                    self.open = true;
                }
                if is_ident(object, "Reflect") {
                    self.reflect_members.insert(object.span);
                }
            }
            ast::ExprKind::Object(props) => {
                for p in props {
                    match p {
                        ast::ObjectProp::KeyValue(name, value) if name.name == "get" => {
                            if let ast::ExprKind::Arrow { params, body, .. } = &value.kind {
                                let key = params
                                    .get(1)
                                    .filter(|k| k.pattern.is_none() && !k.rest)
                                    .map(|k| k.name.name.as_str());
                                self.trap(key, &mut |t| match body {
                                    ast::ArrowBody::Expr(b) => walk_expr(b, t),
                                    ast::ArrowBody::Block(b) => walk_block(b, t),
                                });
                            }
                        }
                        ast::ObjectProp::Method(f) if f.sig.name.name == "get" => {
                            let key = f
                                .sig
                                .params
                                .get(1)
                                .filter(|k| k.pattern.is_none() && !k.rest)
                                .map(|k| k.name.name.as_str());
                            self.trap(key, &mut |t| walk_block(&f.body, t));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// What sema saw while checking bodies.
#[derive(Default)]
pub(crate) struct MethodFacts {
    /// The program's reads through `any` by a computed key.
    pub(crate) reads: Vec<Span>,
    /// One of its reads has a literal key naming an array method.
    pub(crate) named: bool,
    /// The literal names its own code calls through `any` (`Program::any_call_names`).
    pub(crate) call_names: std::collections::BTreeSet<String>,
}

/// `Program::array_methods_open` (module docs).
fn array_methods_open(cx: &Ctx) -> bool {
    if cx.prelude_adt("Proxy").is_none() {
        return false;
    }
    let (reads, named) = (&cx.method_facts.reads, cx.method_facts.named);
    if named {
        return true;
    }
    let mut s = Scan::default();
    for m in cx.modules.iter().filter(|m| !m.is_std) {
        walk_module(&m.ast, &mut s);
    }
    s.open
        || s.reflect_uses
            .iter()
            .any(|r| !s.reflect_members.contains(r))
        || s.gets.iter().any(|g| !s.get_calls.contains(g))
        || s.reflect_reads.iter().any(|r| !s.forwards.contains(r))
        || reads.iter().any(|r| !s.forwards.contains(r))
}

/// `Program::array_methods_open` and `Program::any_call_names`.
pub(crate) fn finish(cx: &mut Ctx) -> (bool, Vec<String>) {
    let open = array_methods_open(cx);
    let names = std::mem::take(&mut cx.method_facts.call_names);
    (open, names.into_iter().collect())
}
