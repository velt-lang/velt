//! Consuming an iterable (docs/internals/design/iteration.md "#424"): spread into an array
//! literal or a rest parameter (`[...gen()]`, `f(...it)`), `Array.from(iterable[, f])`, array
//! destructuring (`const [a, b] = gen()`) and `new Map(iterable)` / `new Set(iterable)`. Each
//! is a `for...of` loop over the source built by the existing loop code (`loops.rs`,
//! `for_iter.rs`), so a direct generator call keeps its state embedded in the frame (no
//! generator object) and any other iterable goes through the protocol, closing the iterator
//! when the loop stops early. The loop body is synthesized source over hidden locals:
//!
//! ```text
//! {
//!   let <array#N> = [];                  // with_capacity(0), typed T[]
//!   for (const <value#N> of src) {
//!     <array#N>.push(<value#N>);
//!     if (<array#N>.length >= n) break;  // destructuring: only the values the pattern needs
//!   }
//!   <array#N>
//! }
//! ```
//!
//! Arrays keep their own spread and destructuring code (`expr/spread.rs`, `pattern.rs`).
//! Strings, `Map`s and `entries()` classes, which `for...of` iterates through the array of
//! their characters or entries (`loops.rs`), are that new array here too: consumers take it
//! as it is (no loop) or build the array loop over it, never the protocol.

use velt_common::Span;
use velt_syntax::ast::{self, Expr, ExprKind as E};

use super::for_iter::{ForOfParts, IterSource};
use super::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, Intrinsic, StmtKind as S, TyId, TyKind, UseMode};

/// A checked source prepared for a synthesized `for...of`, and the type of its values.
pub(super) struct Consumable {
    src: Source,
    pub elem: TyId,
    span: Span,
}

enum Source {
    /// An iterable (`[Symbol.iterator]()`, a generator).
    Iter(IterSource),
    /// An array.
    Array(hir::Expr),
    /// A new array: a string's characters, a `Map`'s or `entries()` class's entries.
    Fresh(hir::Expr),
}

impl Consumable {
    /// Is the source the new array of a string's characters or a `Map`'s entries, which a
    /// consumer may take as it is (`into_fresh`)?
    pub(super) fn is_fresh(&self) -> bool {
        matches!(self.src, Source::Fresh(_))
    }

    /// The new array of a fresh source (`is_fresh`).
    pub(super) fn into_fresh(self) -> hir::Expr {
        match self.src {
            Source::Fresh(h) => h,
            _ => panic!("ICE: not a fresh array source"),
        }
    }
}

impl FnCx<'_, '_> {
    /// Does a consumer take a value of type `t` through `consumable` rather than as an array:
    /// is it an iterable, a string, a `Map` or a class with `entries()` (what `for...of` takes
    /// besides arrays)?
    pub(super) fn is_consumable(&mut self, t: TyId) -> bool {
        self.cx.ty.array_elem(t).is_none()
            && (t == self.cx.ty.str_
                || self.is_iterable(t)
                || self.cx.class_of(t).is_some() && self.method_exists(t, "entries"))
    }

    /// `src` as a source of values, or `None` when `for...of` cannot iterate it (nothing is
    /// reported then, unless the iterable itself is ill-formed).
    pub(super) fn consumable(&mut self, src: hir::Expr) -> Option<Consumable> {
        let span = src.span;
        if self.is_iterable(src.ty) {
            let s = self.iter_source(src, span)?;
            let elem = self.iter_source_elem(&s)?;
            return Some(Consumable {
                src: Source::Iter(s),
                elem,
                span,
            });
        }
        let (src, fresh) =
            if self.cx.class_of(src.ty).is_some() && self.method_exists(src.ty, "entries") {
                (self.entries_of(src, span), true)
            } else if src.ty == self.cx.ty.str_ {
                (self.chars_of(src, span), true)
            } else {
                (src, false)
            };
        let elem = self.cx.ty.array_elem(src.ty)?;
        let src = match fresh {
            true => Source::Fresh(src),
            false => Source::Array(src),
        };
        Some(Consumable { src, elem, span })
    }

    /// `for (const <value> of c) { body }` (`body` names the value `syn.value`).
    pub(super) fn consume(
        &mut self,
        c: Consumable,
        syn: &Synth,
        body: Vec<ast::Stmt>,
        out: &mut Vec<hir::Stmt>,
    ) {
        let pattern = syn.ident_pat(&syn.value);
        let body = syn.block(body);
        let parts = ForOfParts {
            kind: ast::VarKind::Const,
            pattern: &pattern,
            body: &body,
            label: None,
            iter_span: c.span,
            span: syn.span,
            await_each: false,
        };
        match c.src {
            Source::Iter(s) => self.iter_source_loop(s, parts, out),
            Source::Array(h) | Source::Fresh(h) => self.for_of(h, parts, out),
        }
    }

    /// The values of `c` as a new array (at most `limit` of them: the loop stops there, which
    /// closes the iterator), per the module docs. A string's characters or a `Map`'s entries
    /// are already one (all of them: only an iterator could observe stopping early).
    pub(super) fn collect(&mut self, c: Consumable, limit: Option<usize>) -> hir::Expr {
        if c.is_fresh() {
            return c.into_fresh();
        }
        let span = c.span;
        let arr_ty = self.cx.ty.array(c.elem);
        self.push_scope_until(span.hi);
        let syn = Synth::new(span, self.f.locals.len());
        let mut stmts = vec![];
        let out = self.hidden_array(&syn, arr_ty, &mut stmts);
        let push = syn.method(syn.name(&syn.array), "push", vec![syn.name(&syn.value)]);
        let mut body = vec![syn.expr_stmt(push)];
        if let Some(n) = limit {
            let full = syn.binary(
                ast::BinaryOp::GtEq,
                syn.member(syn.name(&syn.array), "length"),
                syn.int(n),
            );
            let stop = syn.if_break(full);
            match n {
                0 => body = vec![stop],
                _ => body.push(stop),
            }
        }
        self.consume(c, &syn, body, &mut stmts);
        self.pop_scope();
        let value = self.mk(H::Local(out, UseMode::Move), arr_ty, span);
        self.with_lets(stmts, value)
    }

    /// `let <array#N> = [];` of type `arr_ty`, visible to synthesized source by name.
    pub(super) fn hidden_array(
        &mut self,
        syn: &Synth,
        arr_ty: TyId,
        out: &mut Vec<hir::Stmt>,
    ) -> hir::LocalId {
        let usize_ = self.cx.ty.usize;
        let zero = self.mk(H::Lit(hir::Lit::Int(0)), usize_, syn.at);
        let init = self.intrinsic(Intrinsic::ArrayWithCapacity, vec![zero], arr_ty, syn.at);
        self.hidden_local(syn.ident(&syn.array), init, true, out)
    }

    /// `let name = init;` (hidden: the name cannot be written in source).
    pub(super) fn hidden_local(
        &mut self,
        name: ast::Ident,
        init: hir::Expr,
        mutable: bool,
        out: &mut Vec<hir::Stmt>,
    ) -> hir::LocalId {
        let span = init.span;
        let local = self.declare_local_mut(&name, init.ty, LocalKind::Let, mutable);
        out.push(hir::Stmt {
            kind: S::Let {
                local,
                init: Some(init),
            },
            span,
        });
        local
    }

    /// The value an array pattern `p` takes apart, for the checked `init`: a non-array iterable
    /// becomes the array of the values the pattern needs (all of them with `...rest`), taken
    /// lazily and closing the iterator after the last, as JS does; a string or a `Map` the
    /// array of its characters or entries. Anything else is `init`.
    pub(super) fn destructured(&mut self, p: &ast::Pattern, init: hir::Expr) -> hir::Expr {
        let ast::PatternKind::Array { elems, rest } = &p.kind else {
            return init;
        };
        if !self.is_consumable(init.ty) {
            return init;
        }
        let span = init.span;
        match self.consumable(init) {
            Some(c) => self.collect(c, rest.is_none().then_some(elems.len())),
            None => self.error_expr(span),
        }
    }

    /// An argument of `new Map(...)` / `new Set(...)` (`T[]` parameter `param`) that is a
    /// non-array iterable: its values as an array.
    pub(super) fn collected_arg(&mut self, h: hir::Expr, param: TyId) -> hir::Expr {
        if self.cx.ty.array_elem(param).is_none() || !self.is_consumable(h.ty) {
            return h;
        }
        let span = h.span;
        match self.consumable(h) {
            Some(c) => self.collect(c, None),
            None => self.error_expr(span),
        }
    }

    /// `Array.from(src)` / `Array.from(src, f)` over any `for...of` source: its values (each
    /// `f(value, index)`) as a new array. `None` when `src` is not iterable.
    pub(super) fn array_from_iterable(
        &mut self,
        src: &Expr,
        f: Option<&Expr>,
        span: Span,
    ) -> Option<hir::Expr> {
        let h = self.expr(src, None, Want::Borrow);
        let ty = h.ty;
        let Some(c) = self.consumable(h) else {
            if let Some(f) = f {
                self.expr(f, None, Want::Borrow);
            }
            return self.cx.ty.is_bottom(ty).then(|| self.error_expr(span));
        };
        let Some(f) = f else {
            return Some(self.collect(c, None));
        };
        Some(self.array_from_mapped(c, f, span))
    }

    /// `Array.from(src, f)`: `{ let i = 0; for (const v of src) { out.push(f(v, i)); i++; } }`.
    fn array_from_mapped(&mut self, c: Consumable, f: &Expr, span: Span) -> hir::Expr {
        let i64_ = self.cx.ty.i64;
        let error = self.cx.ty.error;
        // The result and error types are inferred from the callback.
        let expected = self.cx.ty.intern(TyKind::FnPtr {
            params: vec![c.elem, i64_],
            ret: error,
            throws: error,
        });
        let fh = match as_arrow(f) {
            Some(a) => {
                self.std_callback = true;
                let h = self.closure(a, Some(expected), false);
                self.std_callback = false;
                h
            }
            None => self.expr(f, Some(expected), Want::Borrow),
        };
        let ret = match self.cx.ty.kind(fh.ty) {
            TyKind::FnPtr { ret, .. } => *ret,
            _ if self.cx.ty.is_bottom(fh.ty) => return self.error_expr(span),
            _ => {
                let tn = self.cx.display(fh.ty);
                self.cx.err(
                    format!("the second argument of `Array.from` must be a function, found `{tn}`"),
                    f.span,
                );
                return self.error_expr(span);
            }
        };
        let arr_ty = self.cx.ty.array(ret);
        self.push_scope_until(span.hi);
        let syn = Synth::new(span, self.f.locals.len());
        let mut stmts = vec![];
        self.hidden_local(syn.ident(&syn.map), fh, false, &mut stmts);
        let zero = self.mk(H::Lit(hir::Lit::Int(0)), i64_, syn.at);
        self.hidden_local(syn.ident(&syn.index), zero, true, &mut stmts);
        let out = self.hidden_array(&syn, arr_ty, &mut stmts);
        let mapped = syn.call(
            syn.name(&syn.map),
            vec![syn.name(&syn.value), syn.name(&syn.index)],
        );
        let push = syn.method(syn.name(&syn.array), "push", vec![mapped]);
        let step = syn.expr(E::Update {
            op: ast::UpdateOp::Inc,
            prefix: false,
            target: Box::new(syn.name(&syn.index)),
        });
        let body = vec![syn.expr_stmt(push), syn.expr_stmt(step)];
        self.consume(c, &syn, body, &mut stmts);
        self.pop_scope();
        let value = self.mk(H::Local(out, UseMode::Move), arr_ty, span);
        self.with_lets(stmts, value)
    }
}

/// Builds the synthesized source of one consumer: hidden names (unique per function: numbered
/// by its locals so far) and AST nodes placed at the consumer's start.
pub(super) struct Synth {
    pub array: String,
    pub value: String,
    map: String,
    index: String,
    /// The consumer: the span of synthesized blocks.
    pub span: Span,
    /// Empty, at the consumer's start: synthesized expressions and statements.
    pub at: Span,
}

impl Synth {
    pub(super) fn new(span: Span, n: usize) -> Self {
        Synth {
            array: format!("<array#{n}>"),
            value: format!("<value#{n}>"),
            map: format!("<map#{n}>"),
            index: format!("<index#{n}>"),
            span,
            at: Span::new(span.file, span.lo, span.lo),
        }
    }

    pub(super) fn expr(&self, kind: E) -> Expr {
        Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span: self.at,
        }
    }

    pub(super) fn ident(&self, name: &str) -> ast::Ident {
        ast::Ident {
            name: name.to_string(),
            span: self.at,
        }
    }

    pub(super) fn name(&self, name: &str) -> Expr {
        self.expr(E::Ident(self.ident(name)))
    }

    /// The type named `name` (a builtin like `f64`).
    pub(super) fn named_type(&self, name: &str) -> ast::TypeExpr {
        ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![self.ident(name)],
                args: vec![],
            },
            span: self.at,
        }
    }

    pub(super) fn ident_pat(&self, name: &str) -> ast::Pattern {
        ast::Pattern {
            id: ast::NodeId(u32::MAX),
            kind: ast::PatternKind::Ident(self.ident(name)),
            span: self.at,
        }
    }

    fn int(&self, n: usize) -> Expr {
        self.expr(E::Lit(ast::Lit::Int {
            value: n as u128,
            suffix: None,
        }))
    }

    pub(super) fn member(&self, object: Expr, prop: &str) -> Expr {
        self.expr(E::Member {
            object: Box::new(object),
            prop: self.ident(prop),
            optional: false,
        })
    }

    pub(super) fn call(&self, callee: Expr, args: Vec<Expr>) -> Expr {
        self.expr(E::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args,
            optional: false,
        })
    }

    /// `object.method(args)`
    pub(super) fn method(&self, object: Expr, method: &str, args: Vec<Expr>) -> Expr {
        self.call(self.member(object, method), args)
    }

    fn binary(&self, op: ast::BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
        self.expr(E::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        })
    }

    fn stmt(&self, kind: ast::StmtKind) -> ast::Stmt {
        ast::Stmt {
            kind,
            span: self.at,
        }
    }

    pub(super) fn expr_stmt(&self, e: Expr) -> ast::Stmt {
        self.stmt(ast::StmtKind::Expr(e))
    }

    pub(super) fn block(&self, stmts: Vec<ast::Stmt>) -> ast::Block {
        ast::Block {
            stmts,
            span: self.span,
        }
    }

    /// `if (cond) break;`
    fn if_break(&self, cond: Expr) -> ast::Stmt {
        self.stmt(ast::StmtKind::If {
            cond,
            then: self.block(vec![self.stmt(ast::StmtKind::Break(None))]),
            els: None,
        })
    }
}

/// The arrow function in `e` (possibly parenthesized).
fn as_arrow(e: &Expr) -> Option<&Expr> {
    match &e.kind {
        E::Arrow { .. } => Some(e),
        E::Paren(x) => as_arrow(x),
        _ => None,
    }
}
