//! Destructuring assignment (#1227): `[a, b] = [b, a]`, `({ x, y: o.y, z = 0 } = p)`,
//! `[first, ...rest] = xs`, nested and with defaults, as in JavaScript. The target is taken apart
//! by a declaration of hidden variables, which then are assigned to the places the target
//! names, in order:
//!
//! ```text
//! { const [<d0>, { y: <d1>, z: <d2> = 0 }] = value;
//!   a = <d0>; o.y = <d1>; z = <d2>; }
//! ```
//!
//! so the defaults, the rest and the checks are those of declarations (`pattern_defaults`,
//! `object_rest`). A default is evaluated before any place is assigned (JavaScript assigns each
//! place before evaluating the next default). The assignment has no value: used as one, it is
//! reported like an assignment to an element.
//!
//! `{ a = 1 }` (TypeScript's shorthand property initializer) parses as `a: a = 1` with the
//! assignment starting at the key; outside an assignment pattern it is an error (TS1312).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H};

/// Is `p` the shorthand property initializer `a = 1` (`{ a = 1 }`, module docs)?
pub(super) fn cover_init(p: &ast::ObjectProp) -> Option<(&ast::Ident, &ast::Expr)> {
    let ast::ObjectProp::KeyValue(k, v) = p else {
        return None;
    };
    match &v.kind {
        ast::ExprKind::Assign {
            op: None,
            target,
            value,
        } if v.span.lo == k.span.lo
            && matches!(&target.kind, ast::ExprKind::Ident(t) if t.name == k.name) =>
        {
            Some((k, value))
        }
        _ => None,
    }
}

/// A target turned into a pattern: the pattern, and the places its hidden variables go to.
struct Split {
    places: Vec<(String, ast::Expr)>,
    at: u32,
}

impl Split {
    fn hidden(&mut self, place: &ast::Expr) -> ast::Pattern {
        let name = format!("<destructured {} {}>", self.at, self.places.len());
        let span = place.span;
        self.places.push((name.clone(), place.clone()));
        pattern_of(ast::PatternKind::Ident(ident(&name, span)), span)
    }

    /// The pattern for target `t`, or `None` for a target no pattern can express.
    fn pattern(&mut self, t: &ast::Expr) -> Option<ast::Pattern> {
        use ast::ExprKind as E;
        let kind = match &t.kind {
            E::Paren(inner) => return self.pattern(inner),
            E::Ident(_) | E::Member { .. } | E::Index { .. } => return Some(self.hidden(t)),
            E::Assign {
                op: None,
                target,
                value,
            } => ast::PatternKind::Default {
                pattern: Box::new(self.pattern(target)?),
                value: value.clone(),
            },
            E::Array(items) => {
                let mut elems = vec![];
                let mut rest = None;
                for (i, x) in items.iter().enumerate() {
                    match &x.kind {
                        E::Spread(r) if i + 1 == items.len() => {
                            rest = Some(self.rest(r)?);
                        }
                        _ => elems.push(self.pattern(x)?),
                    }
                }
                ast::PatternKind::Array { elems, rest }
            }
            E::Object(props) => {
                let mut fields = vec![];
                let mut rest = None;
                for (i, p) in props.iter().enumerate() {
                    match p {
                        _ if cover_init(p).is_some() => {
                            let (k, value) = cover_init(p)?;
                            let ast::ObjectProp::KeyValue(_, v) = p else {
                                return None;
                            };
                            let ast::ExprKind::Assign { target, .. } = &v.kind else {
                                return None;
                            };
                            let inner = self.hidden(target);
                            let kind = ast::PatternKind::Default {
                                pattern: Box::new(inner),
                                value: Box::new(value.clone()),
                            };
                            fields.push((k.clone(), pattern_of(kind, v.span)));
                        }
                        ast::ObjectProp::KeyValue(k, v) => {
                            fields.push((k.clone(), self.pattern(v)?))
                        }
                        ast::ObjectProp::Shorthand(k) => {
                            let place = expr_of(E::Ident(k.clone()), k.span);
                            fields.push((k.clone(), self.hidden(&place)));
                        }
                        ast::ObjectProp::Spread(r) if i + 1 == props.len() => {
                            rest = Some(self.rest(r)?);
                        }
                        _ => return None,
                    }
                }
                ast::PatternKind::Object { fields, rest }
            }
            _ => return None,
        };
        Some(pattern_of(kind, t.span))
    }

    /// `...r`: a hidden rest variable, assigned to `r` afterwards.
    fn rest(&mut self, r: &ast::Expr) -> Option<ast::Ident> {
        let p = self.pattern(r)?;
        match p.kind {
            ast::PatternKind::Ident(id) => Some(id),
            _ => None,
        }
    }
}

fn ident(name: &str, span: Span) -> ast::Ident {
    ast::Ident {
        name: name.into(),
        span,
    }
}

fn pattern_of(kind: ast::PatternKind, span: Span) -> ast::Pattern {
    ast::Pattern {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn expr_of(kind: ast::ExprKind, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

impl FnCx<'_, '_> {
    /// `target = value` with an array or object literal as the target (module docs); `None`
    /// for any other target.
    pub(super) fn pattern_assign(
        &mut self,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        let mut inner = target;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        if op.is_some()
            || !matches!(
                inner.kind,
                ast::ExprKind::Array(_) | ast::ExprKind::Object(_)
            )
        {
            return None;
        }
        let mut split = Split {
            places: vec![],
            at: span.lo,
        };
        let pattern = match split.pattern(inner) {
            Some(p) => p,
            None => {
                self.cx.error(
                    Diagnostic::error("this destructuring assignment target is not supported", target.span)
                        .with_note("assign to variables, fields or elements, with defaults and a last `...rest`"),
                );
                self.expr(value, None, crate::body::Want::Borrow);
                return Some(self.error_expr(span));
            }
        };
        let mut stmts = vec![ast::Stmt {
            kind: ast::StmtKind::Var(ast::VarDecl {
                kind: ast::VarKind::Const,
                pattern,
                ty: None,
                init: Some(value.clone()),
                span,
            }),
            span,
        }];
        for (name, place) in &split.places {
            let read = expr_of(ast::ExprKind::Ident(ident(name, place.span)), place.span);
            let assign = ast::ExprKind::Assign {
                op: None,
                target: Box::new(place.clone()),
                value: Box::new(read),
            };
            stmts.push(ast::Stmt {
                kind: ast::StmtKind::Expr(expr_of(assign, place.span)),
                span: place.span,
            });
        }
        let mut out = vec![];
        self.push_scope();
        for s in &stmts {
            self.stmt(s, &mut out);
        }
        self.pop_scope();
        let block = hir::Block {
            stmts: out,
            value: None,
            span,
        };
        Some(self.mk(H::Block(block), self.cx.ty.unit, span))
    }

    /// `{ a = 1 }` outside an assignment pattern (module docs): TypeScript's TS1312.
    pub(super) fn reject_cover_inits(&mut self, props: &[ast::ObjectProp]) {
        for p in props {
            if let Some((k, _)) = cover_init(p) {
                self.cx.err(
                    "`=` after a shorthand property is only allowed in a destructuring assignment (`({ a = 1 } = o)`); did you mean `:`? (TypeScript's TS1312)",
                    k.span,
                );
            }
        }
    }
}
