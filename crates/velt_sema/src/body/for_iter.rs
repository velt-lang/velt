//! `for...of` over an iterable (docs/internals/design/iteration.md): a value whose type has a
//! `[Symbol.iterator]()` method returning an `Iterator<T, E>` (the prelude's protocol,
//! std/prelude/iter.vlt). Arrays, `Map`s and `entries()` classes keep their own loops
//! (`loops.rs`). The loop is desugared here, before checking, into
//!
//! ```text
//! {
//!   let <iterator> = src[Symbol.iterator]();      // once; checked HIR, owned by the loop
//!   let <open> = false;
//!   try {
//!     label: while (true) {
//!       <open> = false;
//!       const <result> = <iterator>.next();       // throws E
//!       if (<result>.done) { break; }
//!       <open> = true;
//!       const pattern = <result>.value;           // moved into the binding
//!       { body }
//!     }
//!   } finally {
//!     if (<open>) { <iterator>.return(); }
//!   }
//! }
//! ```
//!
//! `<open>` is true exactly while the body runs, so leaving the body early (`break`, `return`,
//! a labelled `break`/`continue` to an outer statement, a thrown error) calls `return()` once,
//! and normal completion (`done`) or an error thrown by `next()` does not, as in JS. A
//! `continue` of this loop goes on with `next()`. The hidden names cannot be written in source.
//!
//! A direct generator call (`for (x of gen(a))`, or a class whose `[Symbol.iterator]` is a
//! generator method) skips the protocol: the generator stays in a hidden local whose state
//! lowering embeds in the frame (hir_encodings.md "Generators"):
//!
//! ```text
//! {
//!   let <generator> = GeneratorEmbed(gen(a));
//!   label: while (GeneratorResume(<generator>)) {   // throws E
//!     const pattern = GeneratorValue(<generator>);
//!     { body }
//!   }
//! }                                                 // dropping it closes the generator
//! ```

use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, SYMBOL_ITERATOR};

use super::pattern::BindCtx;
use super::{FnCx, LocalKind};
use crate::defs::ThrowSrc;
use crate::hir::{self, Callee, ExprKind as H, Intrinsic, StmtKind as S, TyId, TyKind, UseMode};

/// How an iterable is iterated (`FnCx::iter_source`).
pub(super) enum IterSource {
    /// A direct generator call (or a generator `[Symbol.iterator]()` method call): its state is
    /// embedded in the frame.
    Embedded(hir::Expr),
    /// The checked `[Symbol.iterator]()` call: the protocol loop.
    Protocol(hir::Expr),
}

/// The source pieces of one `for...of` (or `for await`) statement.
#[derive(Clone, Copy)]
pub(super) struct ForOfParts<'a> {
    pub kind: ast::VarKind,
    pub pattern: &'a ast::Pattern,
    pub body: &'a ast::Block,
    pub label: Option<&'a ast::Ident>,
    /// The iterated expression's span.
    pub iter_span: Span,
    pub span: Span,
    /// `for await` over a sync source: promise values are awaited (`for_await.rs`).
    pub await_each: bool,
}

impl FnCx<'_, '_> {
    /// Can `for...of` iterate a value of type `t` through `[Symbol.iterator]()`? Arrays,
    /// strings and `Map`s are iterable too (std/prelude/iter.vlt), but keep their own loops.
    pub(super) fn is_iterable(&mut self, t: TyId) -> bool {
        self.cx.ty.array_elem(t).is_none()
            && t != self.cx.ty.str_
            && !self.is_prelude_map(t)
            && self.method_exists(t, SYMBOL_ITERATOR)
    }

    /// Is `t` the prelude's `Map<K, V>` (iterated through `entries()`)?
    pub(super) fn is_prelude_map(&self, t: TyId) -> bool {
        matches!(self.cx.ty.kind(t), TyKind::Adt(d, _) if Some(*d) == self.cx.prelude_adt("Map"))
    }

    /// `for (kind pattern of src) body` over the checked iterable `src` (see the module docs).
    pub(super) fn for_of_iterable(
        &mut self,
        src: hir::Expr,
        p: ForOfParts<'_>,
        out: &mut Vec<hir::Stmt>,
    ) {
        match self.iter_source(src, p.iter_span) {
            Some(s) => self.iter_source_loop(s, p, out),
            None => self.check_body_only(&p),
        }
    }

    /// How the checked iterable `src` is iterated: embedded when it is a direct generator call
    /// (or its `[Symbol.iterator]()` is a generator method), else through the protocol. `None`
    /// after an error (reported).
    pub(super) fn iter_source(&mut self, src: hir::Expr, span: Span) -> Option<IterSource> {
        if self.is_generator_call(&src) {
            return Some(IterSource::Embedded(src));
        }
        let call = self.method_call_hir(src, SYMBOL_ITERATOR, span)?;
        if !self.is_iterator(call.ty, span) {
            return None;
        }
        Some(match self.is_generator_call(&call) {
            true => IterSource::Embedded(call),
            false => IterSource::Protocol(call),
        })
    }

    /// The type of the values `s` produces (`None` after an error).
    pub(super) fn iter_source_elem(&mut self, s: &IterSource) -> Option<TyId> {
        let args = match s {
            IterSource::Embedded(call) => self.cx.generator_result(call.ty).map(|(_, a)| a),
            IterSource::Protocol(call) => self.iterator_args(call.ty),
        };
        args.and_then(|a| a.first().copied())
    }

    /// `for (kind pattern of <s>) body`.
    pub(super) fn iter_source_loop(
        &mut self,
        s: IterSource,
        p: ForOfParts<'_>,
        out: &mut Vec<hir::Stmt>,
    ) {
        match s {
            IterSource::Embedded(call) => self.for_of_generator(call, p, out),
            IterSource::Protocol(call) => {
                let await_value = p.await_each && self.yields_promises(call.ty, "Iterator");
                self.protocol_loop(call, p, false, await_value, out);
            }
        }
    }

    /// The protocol loop (module docs) over `call`, the checked `[Symbol.iterator]()` (or, with
    /// `is_async`, `[Symbol.asyncIterator]()`) call: `next()` and `return()` are awaited for an
    /// async iterator, and with `await_value` each value is (a sync iterator of promises under
    /// `for await`).
    pub(super) fn protocol_loop(
        &mut self,
        call: hir::Expr,
        p: ForOfParts<'_>,
        is_async: bool,
        await_value: bool,
        out: &mut Vec<hir::Stmt>,
    ) {
        let span = p.span;
        self.push_scope_until(span.hi);
        let names = Hidden::new(span);
        let it = ast::Ident {
            name: names.iterator.clone(),
            span: p.iter_span,
        };
        let local = self.declare_local_mut(&it, call.ty, LocalKind::Let, true);
        let mut stmts = vec![hir::Stmt {
            kind: S::Let {
                local,
                init: Some(call),
            },
            span: p.iter_span,
        }];
        for s in names.desugar(&p, is_async, await_value) {
            self.stmt(&s, &mut stmts);
        }
        self.pop_scope();
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        Self::push(out, S::Block(block), span);
    }

    /// Does iterator type `t` (an `Iterator<T>` when `iface` is `"Iterator"`) produce promises?
    fn yields_promises(&mut self, t: TyId, iface: &str) -> bool {
        self.iface_args(t, iface)
            .and_then(|a| a.first().copied())
            .is_some_and(|v| matches!(self.cx.ty.kind(v), TyKind::Promise(..)))
    }

    /// A direct call of a (sync) generator function or method (`gen(a)`, `obj.items()`, or a
    /// `*[Symbol.iterator]()` method called by the loop).
    fn is_generator_call(&self, e: &hir::Expr) -> bool {
        matches!(&e.kind, H::Call { callee: Callee::Def(d, _), .. }
            if self.is_generator_fn(*d) && !self.is_async_generator_fn(*d))
    }

    /// A direct call of an async generator function or method.
    pub(super) fn is_async_generator_call(&self, e: &hir::Expr) -> bool {
        matches!(&e.kind, H::Call { callee: Callee::Def(d, _), .. } if self.is_async_generator_fn(*d))
    }

    /// `for (kind pattern of call) body` over a direct generator call (module docs): the
    /// generator lives in a hidden local whose state lowering keeps inline (no heap object, no
    /// `IteratorResult`); leaving the loop drops the local, which closes the generator.
    fn for_of_generator(&mut self, call: hir::Expr, p: ForOfParts<'_>, out: &mut Vec<hir::Stmt>) {
        self.embedded_loop(call, p, false, out);
    }

    /// The embedded loop over a direct (with `is_async`, async) generator call: the sync loop
    /// above, or `for_await.rs`'s.
    pub(super) fn embedded_loop(
        &mut self,
        call: hir::Expr,
        p: ForOfParts<'_>,
        is_async: bool,
        out: &mut Vec<hir::Stmt>,
    ) {
        let span = p.span;
        let Some((_, args)) = self.cx.generator_result(call.ty) else {
            return self.check_body_only(&p);
        };
        let (t, e) = (args[0], args[1]);
        let gen_ty = match is_async {
            true => self.cx.async_generator_ty(t, e),
            false => self.cx.generator_ty(t, e),
        };
        self.push_scope_until(span.hi);
        let names = Hidden::new(span);
        let g = ast::Ident {
            name: names.generator.clone(),
            span: p.iter_span,
        };
        let local = self.declare_local_mut(&g, gen_ty, LocalKind::Let, true);
        let init = self.intrinsic_hir(Intrinsic::GeneratorEmbed, vec![call], gen_ty, p.iter_span);
        let mut stmts = vec![hir::Stmt {
            kind: S::Let {
                local,
                init: Some(init),
            },
            span: p.iter_span,
        }];
        let use_g = |fx: &Self| fx.mk(H::Local(local, UseMode::BorrowMut), gen_ty, names.at);
        let bool_ = self.cx.ty.bool_;
        let resume = match is_async {
            true => {
                let pt = self.cx.ty.promise_rejecting(bool_, e);
                let r = self.intrinsic_hir(
                    Intrinsic::AsyncGeneratorResume,
                    vec![use_g(self)],
                    pt,
                    names.at,
                );
                self.mk(H::Await(Box::new(r)), bool_, names.at)
            }
            false => self.intrinsic_hir(
                Intrinsic::GeneratorResume,
                vec![use_g(self)],
                bool_,
                names.at,
            ),
        };
        if e != self.cx.ty.never {
            self.throw_src(ThrowSrc::Direct(e, p.iter_span));
        }
        let value_i = match is_async {
            true => Intrinsic::AsyncGeneratorValue,
            false => Intrinsic::GeneratorValue,
        };
        let value = self.intrinsic_hir(value_i, vec![use_g(self)], t, names.at);
        let value = self.await_each(value, &p);
        self.push_scope_until(span.hi);
        let value = self.destructured(p.pattern, value);
        let mutable = p.kind == ast::VarKind::Let;
        let ctx = BindCtx::Let {
            mutable,
            place: false,
        };
        let pat = self.pattern(p.pattern, value.ty, ctx);
        self.enter_loop(p.label, false);
        let b = self.block(p.body);
        let label = self.exit_loop().hir_label();
        self.pop_scope();
        let body = hir::Block {
            stmts: vec![
                hir::Stmt {
                    kind: S::LetPat { pat, init: value },
                    span: names.at,
                },
                hir::Stmt {
                    kind: S::Block(b),
                    span: p.body.span,
                },
            ],
            value: None,
            span,
        };
        let lp = S::While {
            label,
            cond: resume,
            body,
            step: None,
        };
        let lp = hir::Stmt { kind: lp, span };
        stmts.push(match is_async {
            true => self.closing(lp, use_g(self), names.at),
            false => lp,
        });
        self.pop_scope();
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        Self::push(out, S::Block(block), span);
    }

    fn intrinsic_hir(&self, i: Intrinsic, args: Vec<hir::Expr>, ty: TyId, span: Span) -> hir::Expr {
        let kind = H::Call {
            callee: Callee::Intrinsic(i),
            args,
        };
        self.mk(kind, ty, span)
    }

    /// After an error: check the body (for its own diagnostics) with the binding untyped.
    pub(super) fn check_body_only(&mut self, p: &ForOfParts<'_>) {
        self.push_scope_until(p.span.hi);
        let mutable = p.kind == ast::VarKind::Let;
        let error = self.cx.ty.error;
        self.pattern(p.pattern, error, BindCtx::Elem { mutable });
        self.enter_loop(p.label, false);
        self.block(p.body);
        self.exit_loop();
        self.pop_scope();
    }

    /// Under `for await` over a sync source, `await value` when it is a promise.
    fn await_each(&mut self, value: hir::Expr, p: &ForOfParts<'_>) -> hir::Expr {
        match self.cx.ty.kind(value.ty).clone() {
            TyKind::Promise(t, _) if p.await_each => self.await_hir(value, t),
            _ => value,
        }
    }

    /// `await e` (`e: Promise<t, E>`), rethrowing its rejection.
    pub(super) fn await_hir(&mut self, e: hir::Expr, t: TyId) -> hir::Expr {
        if let Some(err) = self
            .cx
            .ty
            .promise_error(e.ty)
            .filter(|&x| x != self.cx.ty.never)
        {
            self.throw_src(ThrowSrc::Direct(err, e.span));
        }
        let span = e.span;
        self.mk(H::Await(Box::new(e)), t, span)
    }

    /// `try { lp } finally { await AsyncGeneratorReturn(g) }`: leaving an embedded async
    /// generator loop closes the generator, awaiting its cleanup (a no-op once it is done).
    fn closing(&mut self, lp: hir::Stmt, g: hir::Expr, at: Span) -> hir::Stmt {
        let unit = self.cx.ty.unit;
        let pt = self.cx.ty.promise(unit);
        let close = self.intrinsic_hir(Intrinsic::AsyncGeneratorReturn, vec![g], pt, at);
        let close = self.mk(H::Await(Box::new(close)), unit, at);
        let block = |stmts| hir::Block {
            stmts,
            value: None,
            span: at,
        };
        let fin = hir::Stmt {
            kind: S::Expr(close),
            span: at,
        };
        let span = lp.span;
        hir::Stmt {
            kind: S::Try {
                body: block(vec![lp]),
                catch: None,
                finally: Some(block(vec![fin])),
            },
            span,
        }
    }

    /// Is `t` (what `[Symbol.iterator]()` returns) an `Iterator<T, E>`? Reports it if not.
    fn is_iterator(&mut self, t: TyId, span: Span) -> bool {
        if self.cx.ty.is_bottom(t) || self.iterator_args(t).is_some() {
            return true;
        }
        let tn = self.cx.display(t);
        self.cx.error(
            Diagnostic::error(
                format!("`[Symbol.iterator]()` must return an `Iterator<T>`, found `{tn}`"),
                span,
            )
            .with_note(
                "declare the iterator class with `implements Iterator<T>` and a `next(): IteratorResult<T>` method, or return `Iterator<T>`",
            ),
        );
        false
    }

    /// The type arguments `[T, E]` with which `t` is an `Iterator<T, E>`.
    fn iterator_args(&mut self, t: TyId) -> Option<Vec<TyId>> {
        self.iface_args(t, "Iterator")
    }

    /// The type arguments with which `t` implements (or is) the prelude interface `iface`.
    pub(super) fn iface_args(&mut self, t: TyId, iface: &str) -> Option<Vec<TyId>> {
        let iterator = self.cx.prelude_iface(iface)?;
        if let TyKind::Dyn(d, args) = self.cx.ty.kind(t).clone() {
            if d == iterator {
                return Some(args);
            }
            let parents = self.cx.iface(d)?.parents.clone();
            let p = parents.into_iter().find(|p| p.iface == iterator)?;
            return Some(p.args.iter().map(|a| self.cx.subst(*a, &args)).collect());
        }
        self.cx.impl_args(t, iterator)
    }
}

/// The hidden locals of one desugared loop (unique per statement: named by its offset).
pub(super) struct Hidden {
    iterator: String,
    generator: String,
    open: String,
    result: String,
    /// The `for` statement: the span of synthesized blocks (their scopes end with it).
    span: Span,
    /// Empty, at the `for` keyword: synthesized expressions and statements (diagnostics point
    /// at the loop; editor lookups never land on them).
    at: Span,
}

impl Hidden {
    pub(super) fn new(span: Span) -> Self {
        let at = span.lo;
        Hidden {
            iterator: format!("<iterator@{at}>"),
            generator: format!("<generator@{at}>"),
            open: format!("<open@{at}>"),
            result: format!("<result@{at}>"),
            span,
            at: Span::new(span.file, at, at),
        }
    }

    /// The statements after `let <iterator> = ...` (see the module docs): with `is_async`,
    /// `next()` and `return()` are awaited, and with `await_value` the value is.
    fn desugar(&self, p: &ForOfParts<'_>, is_async: bool, await_value: bool) -> Vec<ast::Stmt> {
        let open = self.var(
            ast::VarKind::Let,
            self.ident_pat(&self.open),
            self.bool_lit(false),
        );
        let next = self.awaited(self.call(&self.iterator, "next"), is_async);
        let result = self.var(ast::VarKind::Const, self.ident_pat(&self.result), next);
        let done = self.member(self.name(&self.result), "done");
        let stop = self.stmt(ast::StmtKind::If {
            cond: done,
            then: self.block(vec![self.stmt(ast::StmtKind::Break(None))]),
            els: None,
        });
        let value = self.member(self.name(&self.result), "value");
        let bind = self.var(p.kind, p.pattern.clone(), self.awaited(value, await_value));
        let body = ast::Stmt {
            kind: ast::StmtKind::Block(p.body.clone()),
            span: p.body.span,
        };
        let steps = vec![
            self.set_open(false),
            result,
            stop,
            self.set_open(true),
            bind,
            body,
        ];
        let mut lp = self.stmt(ast::StmtKind::While {
            cond: self.bool_lit(true),
            body: self.block(steps),
        });
        if let Some(l) = p.label {
            lp = self.stmt(ast::StmtKind::Labeled {
                label: l.clone(),
                body: Box::new(lp),
            });
        }
        let close = self.stmt(ast::StmtKind::If {
            cond: self.name(&self.open),
            then: self.block(vec![
                self.expr_stmt(self.awaited(self.call(&self.iterator, "return"), is_async))
            ]),
            els: None,
        });
        let guarded = self.stmt(ast::StmtKind::Try {
            body: self.block(vec![lp]),
            catch: None,
            finally: Some(self.block(vec![close])),
        });
        vec![open, guarded]
    }

    /// `await e` when `yes`.
    fn awaited(&self, e: ast::Expr, yes: bool) -> ast::Expr {
        match yes {
            true => self.expr(ast::ExprKind::Await(Box::new(e))),
            false => e,
        }
    }

    fn expr(&self, kind: ast::ExprKind) -> ast::Expr {
        ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span: self.at,
        }
    }

    fn stmt(&self, kind: ast::StmtKind) -> ast::Stmt {
        ast::Stmt {
            kind,
            span: self.at,
        }
    }

    fn block(&self, stmts: Vec<ast::Stmt>) -> ast::Block {
        ast::Block {
            stmts,
            span: self.span,
        }
    }

    fn ident(&self, name: &str) -> ast::Ident {
        ast::Ident {
            name: name.to_string(),
            span: self.at,
        }
    }

    fn name(&self, name: &str) -> ast::Expr {
        self.expr(ast::ExprKind::Ident(self.ident(name)))
    }

    fn ident_pat(&self, name: &str) -> ast::Pattern {
        ast::Pattern {
            id: ast::NodeId(u32::MAX),
            kind: ast::PatternKind::Ident(self.ident(name)),
            span: self.at,
        }
    }

    fn bool_lit(&self, b: bool) -> ast::Expr {
        self.expr(ast::ExprKind::Lit(ast::Lit::Bool(b)))
    }

    fn member(&self, object: ast::Expr, prop: &str) -> ast::Expr {
        self.expr(ast::ExprKind::Member {
            object: Box::new(object),
            prop: self.ident(prop),
            optional: false,
        })
    }

    /// `local.method()`
    fn call(&self, local: &str, method: &str) -> ast::Expr {
        self.expr(ast::ExprKind::Call {
            callee: Box::new(self.member(self.name(local), method)),
            type_args: vec![],
            args: vec![],
            optional: false,
        })
    }

    fn expr_stmt(&self, e: ast::Expr) -> ast::Stmt {
        self.stmt(ast::StmtKind::Expr(e))
    }

    fn var(&self, kind: ast::VarKind, pattern: ast::Pattern, init: ast::Expr) -> ast::Stmt {
        self.stmt(ast::StmtKind::Var(ast::VarDecl {
            kind,
            pattern,
            ty: None,
            init: Some(init),
            span: self.at,
        }))
    }

    /// `<open> = b;`
    fn set_open(&self, b: bool) -> ast::Stmt {
        self.expr_stmt(self.expr(ast::ExprKind::Assign {
            op: None,
            target: Box::new(self.name(&self.open)),
            value: Box::new(self.bool_lit(b)),
        }))
    }
}
