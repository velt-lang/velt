//! Iterable object literals (docs/reference/types.md "Iterable object literals"): an object
//! literal whose one member is a `[Symbol.iterator]()` method (`*[Symbol.iterator]()` for a
//! generator; `[Symbol.asyncIterator]()` / `async *[Symbol.asyncIterator]()` for an async
//! iterable). The method is a function value — a generator function expression
//! (`gen_closure.rs`) or an arrow — and the object is the prelude's
//! `__IterableObject<T, E>` (`__AsyncIterableObject<T, E>`) holding it, an `Iterable<T, E>`
//! whose `[Symbol.iterator]()` calls it:
//!
//! ```text
//! { *[Symbol.iterator](): Generator<T> { body } }
//!   →  { const <iterator#N> = function* (): Iterator<T> { body }; new __IterableObject(<iterator#N>) }
//! ```
//!
//! An object literal is plain data in Velt (its type has fields only), so other methods, other
//! members next to the iterator method, and `this` in it are errors that say what to write.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, SYMBOL_ASYNC_ITERATOR, SYMBOL_ITERATOR};

use crate::body::consume::Synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// An object literal with a method (see the module docs).
    pub(super) fn method_object(&mut self, props: &[ast::ObjectProp], span: Span) -> hir::Expr {
        let Some(d) = self.iterator_method(props, span) else {
            return self.error_expr(span);
        };
        let is_async = d.sig.name.name == SYMBOL_ASYNC_ITERATOR;
        let f = match d.sig.is_generator {
            true => self.iterator_generator(d, is_async, span),
            false => self.iterator_function(d, is_async, span),
        };
        let Some(f) = f else {
            return self.error_expr(span);
        };
        self.push_scope_until(span.hi);
        let syn = Synth::new(span, self.f.locals.len());
        let mut lets = vec![];
        self.hidden_local(syn.ident(&syn.value), f, false, &mut lets);
        let class = match is_async {
            true => "__AsyncIterableObject",
            false => "__IterableObject",
        };
        let new = syn.expr(ast::ExprKind::New {
            class: ast::TypeExpr {
                kind: ast::TypeExprKind::Named {
                    path: vec![syn.ident(class)],
                    args: vec![],
                },
                span: syn.at,
            },
            args: vec![syn.name(&syn.value)],
        });
        let obj = self.expr(&new, None, Want::Move);
        self.pop_scope();
        self.with_lets(lets, obj)
    }

    /// The literal's `[Symbol.iterator]()` / `[Symbol.asyncIterator]()` method, if it is its one
    /// member, takes no parameters and does not use `this`; else reported.
    fn iterator_method<'p>(
        &mut self,
        props: &'p [ast::ObjectProp],
        span: Span,
    ) -> Option<&'p ast::FnDecl> {
        let iterator = |n: &str| n == SYMBOL_ITERATOR || n == SYMBOL_ASYNC_ITERATOR;
        let methods = props.iter().filter_map(|p| match p {
            ast::ObjectProp::Method(d) => Some(d),
            _ => None,
        });
        for d in methods.clone().filter(|d| !iterator(&d.sig.name.name)) {
            self.cx.error(
                Diagnostic::error("methods in object literals are not supported", d.sig.name.span)
                    .with_note("TypeScript allows this; Velt doesn't because an object literal is plain data: its type has fields only (an iterable object literal, `{ *[Symbol.iterator]() { ... } }`, is the exception); write a property holding an arrow function (`name: (x: T): R => ...`), or declare a class"),
            );
        }
        let d = methods.clone().find(|d| iterator(&d.sig.name.name))?;
        if props.len() > 1 {
            self.cx.error(
                Diagnostic::error(
                    format!("an object literal with a `{}()` method cannot have other members", d.sig.name.name),
                    span,
                )
                .with_note("TypeScript allows this; Velt doesn't because the iterable object holds only the method, which cannot reach the other members (`this` is not the object); use variables instead of the other members, or declare a class that `implements Iterable<T>` with fields"),
            );
            return None;
        }
        if let Some(p) = d.sig.params.first() {
            self.cx.err(
                format!("`{}()` takes no parameters", d.sig.name.name),
                p.span,
            );
            return None;
        }
        if let Some(at) = this_in(&d.body) {
            self.cx.error(
                Diagnostic::error(
                    format!("`this` cannot be used in the `{}()` method of an object literal", d.sig.name.name),
                    at,
                )
                .with_note("TypeScript allows this (`this` is the object); Velt doesn't because the object holds only the method, as a function that sees the variables around it; use those variables, or declare a class that `implements Iterable<T>` and reads its fields"),
            );
            return None;
        }
        Some(d)
    }

    /// `*[Symbol.iterator](): Generator<T> { ... }` as a generator function expression whose
    /// result is `Iterator<T, E>` (`AsyncIterator<T, E>`).
    fn iterator_generator(
        &mut self,
        d: &ast::FnDecl,
        is_async: bool,
        span: Span,
    ) -> Option<hir::Expr> {
        let sig = &d.sig;
        if sig.is_async != is_async {
            let fix = match is_async {
                true => "write `async *[Symbol.asyncIterator]()`",
                false => "write `*[Symbol.iterator]()`, or `async *[Symbol.asyncIterator]()` for an async iterable",
            };
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "`{}` does not match the method's key",
                        if sig.is_async { "async *" } else { "*" }
                    ),
                    sig.name.span,
                )
                .with_note(fix),
            );
            return None;
        }
        let ret = match &sig.ret {
            Some(t) => self.resolve(t),
            None => self.cx.ty.unit,
        };
        let (t, e, written) = crate::collect::expr_result_args(self.cx, ret, sig)?;
        let mut declared = sig.throws.as_ref().map(|t| self.resolve(t));
        if written {
            declared = self.cx.join_errors(declared, Some(e));
        }
        let iface = self.iterator_iface(t, e, is_async)?;
        Some(self.gen_closure(d, (iface, t), declared, span))
    }

    /// `[Symbol.iterator](): Iterator<T> { ... }` as an arrow function.
    fn iterator_function(
        &mut self,
        d: &ast::FnDecl,
        is_async: bool,
        span: Span,
    ) -> Option<hir::Expr> {
        let sig = &d.sig;
        let want = if is_async {
            "AsyncIterator"
        } else {
            "Iterator"
        };
        let ret = sig.ret.as_ref().map(|t| self.resolve(t));
        let ok = ret.is_some_and(|r| {
            matches!(self.cx.ty.kind(r), TyKind::Dyn(..)) && self.iface_args(r, want).is_some()
        });
        if !ok {
            if !ret.is_some_and(|r| self.cx.ty.is_bottom(r)) {
                self.cx.error(
                    Diagnostic::error(
                        format!("the `{}()` method of an object literal must return `{want}<T>`", sig.name.name),
                        sig.ret.as_ref().map_or(sig.name.span, |t| t.span),
                    )
                    .with_note(format!("declare its result as `{want}<T>` (an iterator class is returned as one), or make it a generator: `*{}(): Generator<T>`", sig.name.name)),
                );
            }
            return None;
        }
        let arrow = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Arrow {
                type_params: vec![],
                params: vec![],
                ret: sig.ret.clone(),
                throws: sig.throws.clone(),
                body: ast::ArrowBody::Block(d.body.clone()),
                is_async: false,
            },
            span,
        };
        Some(self.closure(&arrow, None, true))
    }

    /// `Iterator<t, e>` (`AsyncIterator<t, e>`) as an interface type.
    fn iterator_iface(&mut self, t: TyId, e: TyId, is_async: bool) -> Option<TyId> {
        let name = if is_async {
            "AsyncIterator"
        } else {
            "Iterator"
        };
        let d = self.cx.prelude_iface(name)?;
        Some(self.cx.ty.intern(TyKind::Dyn(d, vec![t, e])))
    }
}

/// Where `body` uses `this` (nested arrows included: they see the same `this`).
fn this_in(body: &ast::Block) -> Option<Span> {
    struct Find(Option<Span>);
    impl<'a> velt_syntax::visit::Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            if matches!(e.kind, ast::ExprKind::This) && self.0.is_none() {
                self.0 = Some(e.span);
            }
        }
    }
    let wrapped = ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Arrow {
            type_params: vec![],
            params: vec![],
            ret: None,
            throws: None,
            body: ast::ArrowBody::Block(body.clone()),
            is_async: false,
        },
        span: body.span,
    };
    let mut f = Find(None);
    velt_syntax::visit::walk_expr(&wrapped, &mut f);
    f.0
}
