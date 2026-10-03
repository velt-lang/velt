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

/// The source pieces of one `for...of` statement.
pub(super) struct ForOfParts<'a> {
    pub kind: ast::VarKind,
    pub pattern: &'a ast::Pattern,
    pub body: &'a ast::Block,
    pub label: Option<&'a ast::Ident>,
    /// The iterated expression's span.
    pub iter_span: Span,
    pub span: Span,
}

impl FnCx<'_, '_> {
    /// Can `for...of` iterate a value of type `t` through `[Symbol.iterator]()`?
    pub(super) fn is_iterable(&mut self, t: TyId) -> bool {
        self.cx.ty.array_elem(t).is_none() && self.method_exists(t, SYMBOL_ITERATOR)
    }

    /// `for (kind pattern of src) body` over the checked iterable `src` (see the module docs).
    pub(super) fn for_of_iterable(
        &mut self,
        src: hir::Expr,
        p: ForOfParts<'_>,
        out: &mut Vec<hir::Stmt>,
    ) {
        let span = p.span;
        if self.is_generator_call(&src) {
            return self.for_of_generator(src, p, out);
        }
        let call = self.method_call_hir(src, SYMBOL_ITERATOR, p.iter_span);
        let Some(call) = call.filter(|c| self.is_iterator(c.ty, p.iter_span)) else {
            return self.check_body_only(&p);
        };
        if self.is_generator_call(&call) {
            return self.for_of_generator(call, p, out);
        }
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
        for s in names.desugar(&p) {
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

    /// A direct call of a generator function or method (`gen(a)`, `obj.items()`, or a
    /// `*[Symbol.iterator]()` method called by the loop).
    fn is_generator_call(&self, e: &hir::Expr) -> bool {
        matches!(&e.kind, H::Call { callee: Callee::Def(d, _), .. } if self.is_generator_fn(*d))
    }

    /// `for (kind pattern of call) body` over a direct generator call (module docs): the
    /// generator lives in a hidden local whose state lowering keeps inline (no heap object, no
    /// `IteratorResult`); leaving the loop drops the local, which closes the generator.
    fn for_of_generator(&mut self, call: hir::Expr, p: ForOfParts<'_>, out: &mut Vec<hir::Stmt>) {
        let span = p.span;
        let Some((_, args)) = self.cx.generator_result(call.ty) else {
            return self.check_body_only(&p);
        };
        let (t, e) = (args[0], args[1]);
        let gen_ty = self.cx.generator_ty(t, e);
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
        let resume = self.intrinsic_hir(
            Intrinsic::GeneratorResume,
            vec![use_g(self)],
            bool_,
            names.at,
        );
        if e != self.cx.ty.never {
            self.throw_src(ThrowSrc::Direct(e, p.iter_span));
        }
        let value = self.intrinsic_hir(Intrinsic::GeneratorValue, vec![use_g(self)], t, names.at);
        self.push_scope_until(span.hi);
        let mutable = p.kind == ast::VarKind::Let;
        let ctx = BindCtx::Let {
            mutable,
            place: false,
        };
        let pat = self.pattern(p.pattern, t, ctx);
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
        stmts.push(hir::Stmt { kind: lp, span });
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
    fn check_body_only(&mut self, p: &ForOfParts<'_>) {
        self.push_scope_until(p.span.hi);
        let mutable = p.kind == ast::VarKind::Let;
        let error = self.cx.ty.error;
        self.pattern(p.pattern, error, BindCtx::Elem { mutable });
        self.enter_loop(p.label, false);
        self.block(p.body);
        self.exit_loop();
        self.pop_scope();
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
        let iterator = self.cx.prelude_iface("Iterator")?;
        if let TyKind::Dyn(d, args) = self.cx.ty.kind(t).clone() {
            if d == iterator {
                return Some(args);
            }
            let parents = self.cx.iface(d)?.parents.clone();
            let p = parents.into_iter().find(|p| p.iface == iterator)?;
            return Some(p.args.iter().map(|a| self.cx.ty.subst(*a, &args)).collect());
        }
        self.cx.impl_args(t, iterator)
    }
}

/// The hidden locals of one desugared loop (unique per statement: named by its offset).
struct Hidden {
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
    fn new(span: Span) -> Self {
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

    /// The statements after `let <iterator> = ...` (see the module docs).
    fn desugar(&self, p: &ForOfParts<'_>) -> Vec<ast::Stmt> {
        let open = self.var(
            ast::VarKind::Let,
            self.ident_pat(&self.open),
            self.bool_lit(false),
        );
        let next = self.call(&self.iterator, "next");
        let result = self.var(ast::VarKind::Const, self.ident_pat(&self.result), next);
        let done = self.member(self.name(&self.result), "done");
        let stop = self.stmt(ast::StmtKind::If {
            cond: done,
            then: self.block(vec![self.stmt(ast::StmtKind::Break(None))]),
            els: None,
        });
        let value = self.member(self.name(&self.result), "value");
        let bind = self.var(p.kind, p.pattern.clone(), value);
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
            then: self.block(vec![self.expr_stmt(self.call(&self.iterator, "return"))]),
            els: None,
        });
        let guarded = self.stmt(ast::StmtKind::Try {
            body: self.block(vec![lp]),
            catch: None,
            finally: Some(self.block(vec![close])),
        });
        vec![open, guarded]
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
