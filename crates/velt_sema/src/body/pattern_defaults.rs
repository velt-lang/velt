//! Defaults in destructuring declarations (`const { a = 1, b } = opts;`, `const [x = 0] = xs;`).
//! A declaration whose pattern has a default anywhere is rewritten into plain declarations
//! through hidden temporaries, each checked before the next is built, so field and element types
//! are known: a field's default applies when it is `null` (`opts.a ?? 1`), an array element's
//! when the array is too short (`#t.length > 0 ? #t[0] : 0`), where JS reads `undefined`.
//! A non-array iterable is first collected into a second temporary holding the values the
//! pattern needs (`consume.rs`).

use velt_common::Span;
use velt_syntax::ast::{self, Expr, ExprKind as E, Pattern, PatternKind as P, VarDecl, VarKind};

use super::FnCx;
use crate::hir;

/// Does `p` contain a default?
pub(super) fn has_default(p: &Pattern) -> bool {
    match &p.kind {
        P::Default { .. } => true,
        P::Object { fields, .. } => fields.iter().any(|(_, f)| has_default(f)),
        P::Array { elems, .. } => elems.iter().any(has_default),
        P::Ident(_) | P::Wildcard => false,
    }
}

impl FnCx<'_, '_> {
    /// `kind pattern = init;` for a pattern with defaults (see the module docs).
    pub(super) fn decl_with_defaults(
        &mut self,
        kind: VarKind,
        pattern: &Pattern,
        init: Expr,
        out: &mut Vec<hir::Stmt>,
    ) {
        match &pattern.kind {
            P::Object { fields, rest } => {
                if let Some(r) = rest {
                    self.cx
                        .err("`...rest` in object patterns is not supported yet", r.span);
                }
                let tmp = self.temp_decl(init, out);
                for (key, sub) in fields {
                    let field = member(&tmp, key.clone());
                    let (sub, value) = match self.split_default(sub, &tmp, key) {
                        Some((inner, Some(d))) => (inner, nullish(field, d)),
                        Some((inner, None)) => (inner, field),
                        None => (sub, field),
                    };
                    self.decl_with_defaults(kind, sub, value, out);
                }
            }
            P::Array { elems, rest } => {
                let tmp = self.temp_decl(init, out);
                if self.is_tuple_value(&tmp) {
                    // A tuple always has its elements: the defaults never apply.
                    return self.tuple_decl(kind, pattern, tmp, out);
                }
                let tmp = self.collected_temp(tmp, rest.is_none().then_some(elems.len()), out);
                for (k, sub) in elems.iter().enumerate() {
                    let elem = index(&tmp, k);
                    let value = match &sub.kind {
                        P::Default { pattern, value } => {
                            let cond = longer_than(&tmp, k);
                            (pattern.as_ref(), cond_expr(cond, elem, (**value).clone()))
                        }
                        _ => (sub, elem),
                    };
                    self.decl_with_defaults(kind, value.0, value.1, out);
                }
                if let Some(r) = rest {
                    let slice = call(
                        member(&tmp, ident("slice", r.span)),
                        vec![int(elems.len(), r.span)],
                    );
                    let p = pat(P::Ident(r.clone()), r.span);
                    self.decl_with_defaults(kind, &p, slice, out);
                }
            }
            P::Default { pattern, value } => {
                let v = nullish(init, (**value).clone());
                self.decl_with_defaults(kind, pattern, v, out);
            }
            P::Ident(_) | P::Wildcard => {
                let span = pattern.span;
                let v = VarDecl {
                    kind,
                    pattern: pattern.clone(),
                    ty: None,
                    init: Some(init),
                    span,
                };
                self.var_decl(&v, span, out);
            }
        }
    }

    /// `const #dN = init;`: the hidden temporary a pattern is taken apart from.
    fn temp_decl(&mut self, init: Expr, out: &mut Vec<hir::Stmt>) -> Expr {
        self.temp_with(init, None, out)
    }

    /// `const #dN: T = init;` for an annotated declaration with defaults.
    pub(super) fn typed_temp(
        &mut self,
        init: &Expr,
        ty: &ast::TypeExpr,
        out: &mut Vec<hir::Stmt>,
    ) -> Expr {
        self.temp_with(init.clone(), Some(ty.clone()), out)
    }

    fn temp_with(
        &mut self,
        init: Expr,
        ty: Option<ast::TypeExpr>,
        out: &mut Vec<hir::Stmt>,
    ) -> Expr {
        let span = init.span;
        let n = self.f.locals.len();
        let name = ident(&format!("#d{n}"), span);
        let v = VarDecl {
            kind: VarKind::Const,
            pattern: pat(P::Ident(name.clone()), span),
            ty,
            init: Some(init),
            span,
        };
        self.var_decl(&v, span, out);
        mk(E::Ident(name), span)
    }

    /// `{ a = d }` on field `key` of temporary `tmp`: the inner pattern, and the default when
    /// the field can be `null`. On a field that is never `null` the default never applies (TS
    /// accepts it too).
    fn split_default<'p>(
        &mut self,
        sub: &'p Pattern,
        tmp: &Expr,
        key: &ast::Ident,
    ) -> Option<(&'p Pattern, Option<Expr>)> {
        let P::Default { pattern, value } = &sub.kind else {
            return None;
        };
        let ty = self.temp_ty(tmp)?;
        let nullable = self
            .field_of(ty, &key.name)
            .is_some_and(|(_, t)| self.cx.ty.opt_payload(t).is_some());
        Some((pattern, nullable.then(|| (**value).clone())))
    }

    /// `kind [a, b = d] = tmp;` on a tuple: an ordinary destructuring without the defaults.
    fn tuple_decl(&mut self, kind: VarKind, p: &Pattern, tmp: Expr, out: &mut Vec<hir::Stmt>) {
        let span = p.span;
        let v = VarDecl {
            kind,
            pattern: without_defaults(p),
            ty: None,
            init: Some(tmp),
            span,
        };
        self.var_decl(&v, span, out);
    }

    /// A temporary holding a non-array iterable (`const [a, b = 0] = gen()`): a second one
    /// holding the values the pattern needs (at most `limit`), as an array (`consume.rs`).
    fn collected_temp(
        &mut self,
        tmp: Expr,
        limit: Option<usize>,
        out: &mut Vec<hir::Stmt>,
    ) -> Expr {
        let E::Ident(name) = &tmp.kind else {
            return tmp;
        };
        let Some(local) = self.lookup_local(&name.name, name.span) else {
            return tmp;
        };
        let ty = self.f.locals[local.0 as usize].ty;
        if !self.is_iterable(ty) {
            return tmp;
        }
        let span = tmp.span;
        let src = self.mk(hir::ExprKind::Local(local, hir::UseMode::Borrow), ty, span);
        let values = match self.consumable(src) {
            Some(c) => self.collect(c, limit),
            None => self.error_expr(span),
        };
        let name = ident(&format!("#d{}", self.f.locals.len()), span);
        self.hidden_local(name.clone(), values, false, out);
        mk(E::Ident(name), span)
    }

    fn is_tuple_value(&mut self, tmp: &Expr) -> bool {
        self.temp_ty(tmp)
            .is_some_and(|t| matches!(self.cx.ty.kind(t), hir::TyKind::Tuple(_)))
    }

    /// The type of a temporary declared by `temp_decl`.
    fn temp_ty(&mut self, tmp: &Expr) -> Option<hir::TyId> {
        let E::Ident(name) = &tmp.kind else {
            return None;
        };
        let local = self.lookup_local(&name.name, name.span)?;
        Some(self.f.locals[local.0 as usize].ty)
    }
}

/// `p` with every default removed (`[a, b = 1]` → `[a, b]`).
fn without_defaults(p: &Pattern) -> Pattern {
    let kind = match &p.kind {
        P::Default { pattern, .. } => return without_defaults(pattern),
        P::Object { fields, rest } => P::Object {
            fields: fields
                .iter()
                .map(|(k, f)| (k.clone(), without_defaults(f)))
                .collect(),
            rest: rest.clone(),
        },
        P::Array { elems, rest } => P::Array {
            elems: elems.iter().map(without_defaults).collect(),
            rest: rest.clone(),
        },
        k => k.clone(),
    };
    pat(kind, p.span)
}

fn mk(kind: E, span: Span) -> Expr {
    Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn pat(kind: P, span: Span) -> Pattern {
    Pattern {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn ident(name: &str, span: Span) -> ast::Ident {
    ast::Ident {
        name: name.to_string(),
        span,
    }
}

fn member(object: &Expr, prop: ast::Ident) -> Expr {
    let span = object.span;
    mk(
        E::Member {
            object: Box::new(object.clone()),
            prop,
            optional: false,
        },
        span,
    )
}

fn int(n: usize, span: Span) -> Expr {
    mk(
        E::Lit(ast::Lit::Int {
            value: n as u128,
            suffix: None,
        }),
        span,
    )
}

fn index(object: &Expr, k: usize) -> Expr {
    let span = object.span;
    mk(
        E::Index {
            object: Box::new(object.clone()),
            index: Box::new(int(k, span)),
            optional: false,
        },
        span,
    )
}

fn call(callee: Expr, args: Vec<Expr>) -> Expr {
    let span = callee.span;
    mk(
        E::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args,
            optional: false,
        },
        span,
    )
}

fn nullish(lhs: Expr, rhs: Expr) -> Expr {
    let span = lhs.span;
    mk(
        E::Binary {
            op: ast::BinaryOp::Nullish,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        span,
    )
}

/// `tmp.length > k`
fn longer_than(tmp: &Expr, k: usize) -> Expr {
    let span = tmp.span;
    mk(
        E::Binary {
            op: ast::BinaryOp::Gt,
            lhs: Box::new(member(tmp, ident("length", span))),
            rhs: Box::new(int(k, span)),
        },
        span,
    )
}

fn cond_expr(cond: Expr, then: Expr, els: Expr) -> Expr {
    let span = then.span;
    mk(
        E::Cond {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        },
        span,
    )
}
