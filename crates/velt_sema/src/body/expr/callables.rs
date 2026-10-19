//! Building and calling functions with properties (#921, `crate::callables`): a value of a
//! callable object type is the object `{ #call: fn, …members }`.
//!
//! - `Object.assign(fn, { a, b })` where a callable object type is expected (an annotation, a
//!   return type, `as T`) is that object, its members checked against the type's.
//! - `const f = (() => …) as Runner;` followed by property writes `f.stop = …;` is one object
//!   built from the function and the written values: TypeScript's way of making such a value
//!   member by member, while a Velt object has every field from the start. The writes taken
//!   are the statements right after the declaration (`stmt.rs`), in order, each giving a field
//!   of the type a value that does not mention `f`. A required member with no write is an
//!   error.
//! - A call of such a value calls its function; it converts to its plain function type by
//!   reading the function (`coerce.rs`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;
use velt_syntax::visit::{walk_expr, Visit};

use crate::anon::ShapeField;
use crate::body::{FnCx, Want};
use crate::callables::CALL_FIELD;
use crate::hir::{self, DefId, TyId, TyKind};

/// The property writes after `const f = fn as T;` that the cast may take as members
/// (`Frame::callable_writes`).
#[derive(Clone, Debug)]
pub(crate) struct PendingWrites {
    /// The span of the declaration's initializer (the cast).
    pub cast: Span,
    /// The written property and value of each write after the declaration, in order, with the
    /// statement's position among the statements after it.
    pub writes: Vec<(usize, ast::Ident, ast::Expr)>,
    /// The positions of the writes the cast took (the statements to skip).
    pub used: Vec<usize>,
}

impl FnCx<'_, '_> {
    /// The function of callable object `f` (its hidden field), borrowed for a call.
    pub(super) fn call_of_callable(&mut self, f: hir::Expr, span: Span) -> Option<hir::Expr> {
        let prop = ast::Ident {
            name: CALL_FIELD.to_string(),
            span: Span::new(f.span.file, f.span.lo, f.span.lo),
        };
        self.field_access(f, &prop, Want::Borrow, span)
    }

    /// A callable object `h` as its plain function type: its function, shared.
    pub(super) fn callable_as_fn(&mut self, h: hir::Expr) -> hir::Expr {
        let span = h.span;
        let prop = ast::Ident {
            name: CALL_FIELD.to_string(),
            span: Span::new(span.file, span.lo, span.lo),
        };
        match self.field_access(h, &prop, Want::Borrow, span) {
            Some(f) => self.share_if_place(f),
            None => self.error_expr(span),
        }
    }

    /// `e as T` with a callable object type `T` and an `e` that is a function expression or
    /// `Object.assign(fn, { … })` (also behind `as unknown`): the object, or `None` for
    /// another cast.
    pub(super) fn callable_cast(
        &mut self,
        expr: &ast::Expr,
        target: TyId,
        span: Span,
    ) -> Option<hir::Expr> {
        if !self.cx.is_callable_object(target) {
            return None;
        }
        let inner = strip_assertions(expr);
        if let Some(h) = self.callable_assign(inner, Some(target), span) {
            return Some(h);
        }
        if !matches!(
            inner.kind,
            ast::ExprKind::Arrow { .. } | ast::ExprKind::Function(_)
        ) {
            return None;
        }
        let mut props = vec![call_prop(inner)];
        let at = self.f.callable_writes.as_ref().map(|p| p.cast);
        if at == Some(span) {
            props.extend(self.take_writes(target, span));
        }
        Some(self.object_lit(&props, Some(target), span))
    }

    /// `Object.assign(fn, { … })` (`e`) where callable object type `exp` is expected: the
    /// object of that type. `None`: another expression, or no such type is expected.
    pub(super) fn callable_assign(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let exp = exp.filter(|t| self.cx.is_callable_object(*t));
        let ast::ExprKind::Call {
            callee,
            type_args,
            args,
            optional: false,
        } = &e.kind
        else {
            return None;
        };
        if !type_args.is_empty() || !self.object_assign_callee(callee) {
            return None;
        }
        let [target, source] = &args[..] else {
            return None;
        };
        let ast::ExprKind::Object(members) = &strip_parens(source).kind else {
            return None;
        };
        if members.iter().any(|m| {
            !matches!(
                m,
                ast::ObjectProp::KeyValue(..) | ast::ObjectProp::Shorthand(_)
            )
        }) {
            return None;
        }
        let target = strip_parens(target);
        self.note_assign_target(target, span);
        let Some(exp) = exp else {
            return self.inferred_callable(target, members, span);
        };
        let mut props = vec![call_prop(target)];
        props.extend(members.iter().cloned());
        Some(self.object_lit(&props, Some(exp), span))
    }

    /// `Object.assign(f, { … })` (at `span`) onto a variable `f`: remembered, so that a later
    /// use of `f` is reported (`reused_assign_targets`).
    fn note_assign_target(&mut self, target: &ast::Expr, span: Span) {
        if !matches!(target.kind, ast::ExprKind::Ident(_)) {
            return;
        }
        let probe = self.expr(target, None, Want::Borrow);
        if let hir::ExprKind::Local(l, _) = probe.kind {
            if matches!(self.cx.ty.kind(probe.ty), TyKind::FnPtr { .. }) {
                self.f.assign_targets.push((l, span));
            }
        }
    }

    /// The uses in `block` (the body just checked, closures aside) of a variable made a
    /// function with properties by `Object.assign` after that call: reported. JavaScript adds
    /// the properties to the function itself, so the variable and the result are one object;
    /// Velt's result is a new object, which the variable doesn't see.
    pub(crate) fn reused_assign_targets(&mut self, block: &mut hir::Block) {
        let targets = std::mem::take(&mut self.f.assign_targets);
        if targets.is_empty() {
            return;
        }
        let mut uses: Vec<(hir::LocalId, Span)> = vec![];
        crate::visit::exprs_mut(block, &mut |e: &mut hir::Expr| {
            if let hir::ExprKind::Local(l, _) = e.kind {
                uses.push((l, e.span));
            }
        });
        let mut reported = vec![];
        for (l, at) in targets {
            let later = uses
                .iter()
                .filter(|(u, s)| *u == l && s.file == at.file && s.lo >= at.hi)
                .map(|(_, s)| *s)
                .min_by_key(|s| s.lo);
            let Some(used) = later.filter(|_| !reported.contains(&l)) else {
                continue;
            };
            reported.push(l);
            self.cx.error(
                Diagnostic::error(
                    "a function given properties by `Object.assign` is used again after the call: not supported yet (#921)",
                    used,
                )
                .with_note("Velt's `Object.assign(fn, { … })` makes a new function with properties, while JavaScript adds them to `fn` itself; use the result of the call instead of `fn`"),
            );
        }
    }

    /// `Object.assign(f, { … })` with no callable object type expected: the object of
    /// TypeScript's type for it, `F & { … }` (`F` the function's type), when `f` is a function
    /// expression or a variable holding a function.
    fn inferred_callable(
        &mut self,
        target: &ast::Expr,
        members: &[ast::ObjectProp],
        span: Span,
    ) -> Option<hir::Expr> {
        let is_fn = |cx: &mut Self, t: TyId| matches!(cx.cx.ty.kind(t), TyKind::FnPtr { .. });
        match &target.kind {
            ast::ExprKind::Arrow { .. } | ast::ExprKind::Function(_) => {}
            ast::ExprKind::Ident(_) => {
                let probe = self.expr(target, None, Want::Borrow);
                if !is_fn(self, probe.ty) {
                    return None;
                }
            }
            _ => return None,
        }
        let f = self.expr(target, None, Want::Move);
        if !is_fn(self, f.ty) {
            return Some(self.error_expr(span));
        }
        let lit = self.object_lit(members, None, span);
        let hir::ExprKind::AdtLit { fields: values, .. } = lit.kind else {
            return Some(self.error_expr(span));
        };
        let mut fields = vec![ShapeField::plain(CALL_FIELD, f.ty)];
        fields.extend(self.cx.public_fields(lit.ty));
        let (def, type_args) = self.cx.anon_def_with(&fields, self.module);
        let ty = self.cx.ty.intern(TyKind::Adt(def, type_args.clone()));
        let mut hs = vec![f];
        hs.extend(values);
        let kind = hir::ExprKind::AdtLit {
            def,
            type_args,
            fields: hs,
            converted: hir::Conversion::No,
        };
        Some(self.mk(kind, ty, span))
    }

    /// The writes after the declaration being checked that give members of `target` a value
    /// (`PendingWrites`), as object literal properties; the rest are left to run as
    /// statements.
    fn take_writes(&mut self, target: TyId, span: Span) -> Vec<ast::ObjectProp> {
        let Some(mut pending) = self.f.callable_writes.take() else {
            return vec![];
        };
        let fields = self.cx.public_fields(target);
        let mut taken: Vec<ast::ObjectProp> = vec![];
        let mut names: Vec<String> = vec![];
        let mut used = vec![];
        for (at, prop, value) in &pending.writes {
            let member = fields
                .iter()
                .any(|f| f.name == prop.name && f.name != CALL_FIELD);
            if !member || names.contains(&prop.name) {
                break;
            }
            names.push(prop.name.clone());
            used.push(*at);
            taken.push(ast::ObjectProp::KeyValue(prop.clone(), value.clone()));
        }
        pending.used = used;
        pending.cast = span;
        self.f.callable_writes = Some(pending);
        taken
    }

    /// Is `d` a callable object type's def?
    pub(super) fn callable_def(&self, d: DefId) -> bool {
        self.cx
            .adt(d)
            .is_some_and(|a| a.fields.first().is_some_and(|f| f.name == CALL_FIELD))
    }

    /// Callable object type `ty` (shown as `tn`) built at `span` without a value for its
    /// required member `name`.
    pub(super) fn missing_member(&mut self, tn: &str, name: &str, span: Span) {
        self.cx.error(
            Diagnostic::error(format!("missing member `{name}` of `{tn}`"), span).with_note(
                format!("a function cast to `{tn}` gets its members from the property writes right after the declaration (`f.{name} = …;`), or from `Object.assign(fn, {{ {name} }})`"),
            ),
        );
    }
}

/// The note on a property of a function value that has no callable object type.
pub(super) const FN_PROPERTY_NOTE: &str = "a function has no properties of its own here: give it a type with a call signature and the members (`interface F { (): void; x: number }`), cast it and write them right after (`const f = (() => {}) as F; f.x = 1;`)";

/// The property writes `name.p = v;` among `stmts` (the statements after the declaration of
/// `name`), with their positions: up to the first statement that is neither such a write nor
/// the declaration of a function (`const g = () => …`, which runs nothing), or a value that
/// mentions `name` or a function declared in between.
pub(crate) fn property_writes(
    name: &str,
    stmts: &[ast::Stmt],
) -> Vec<(usize, ast::Ident, ast::Expr)> {
    let mut out = vec![];
    let mut declared: Vec<&str> = vec![name];
    for (i, s) in stmts.iter().enumerate() {
        if let Some(f) = function_decl(s) {
            declared.push(f);
            continue;
        }
        let ast::StmtKind::Expr(e) = &s.kind else {
            break;
        };
        let ast::ExprKind::Assign {
            op: None,
            target,
            value,
        } = &e.kind
        else {
            break;
        };
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &target.kind
        else {
            break;
        };
        if !matches!(&object.kind, ast::ExprKind::Ident(i) if i.name == name)
            || declared.iter().any(|n| mentions(value, n))
        {
            break;
        }
        out.push((i, prop.clone(), (**value).clone()));
    }
    out
}

/// The name `s` declares when it is `const g = <arrow or function expression>;`.
fn function_decl(s: &ast::Stmt) -> Option<&str> {
    let ast::StmtKind::Var(v) = &s.kind else {
        return None;
    };
    let ast::PatternKind::Ident(name) = &v.pattern.kind else {
        return None;
    };
    let init = strip_parens(v.init.as_ref()?);
    matches!(
        init.kind,
        ast::ExprKind::Arrow { .. } | ast::ExprKind::Function(_)
    )
    .then_some(name.name.as_str())
}

/// Does `e` mention the name `name` anywhere?
fn mentions(e: &ast::Expr, name: &str) -> bool {
    struct Find<'n>(&'n str, bool);
    impl<'a> Visit<'a> for Find<'_> {
        fn expr(&mut self, e: &'a ast::Expr) {
            if matches!(&e.kind, ast::ExprKind::Ident(i) if i.name == self.0) {
                self.1 = true;
            }
        }
    }
    let mut f = Find(name, false);
    walk_expr(e, &mut f);
    f.1
}

/// The hidden property holding the function `f` in a callable object's literal.
fn call_prop(f: &ast::Expr) -> ast::ObjectProp {
    let key = ast::Ident {
        name: CALL_FIELD.to_string(),
        span: Span::new(f.span.file, f.span.lo, f.span.lo),
    };
    ast::ObjectProp::KeyValue(key, f.clone())
}

fn strip_parens(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) => strip_parens(x),
        _ => e,
    }
}

/// `e` without parentheses and assertions to `unknown` or `any` (`fn as unknown as T`).
fn strip_assertions(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) => strip_assertions(x),
        ast::ExprKind::Cast { expr, ty } if is_top(ty) => strip_assertions(expr),
        _ => e,
    }
}

fn is_top(t: &ast::TypeExpr) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, args } if args.is_empty() && path.len() == 1 && matches!(path[0].name.as_str(), "unknown" | "any"))
}
