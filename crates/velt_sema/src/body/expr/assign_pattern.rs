//! Destructuring assignment (#1227): `[a, b] = [b, a]`, `({ x, y: o.y, z = 0 } = p)`,
//! `[first, ...rest] = xs`, nested and with defaults, as in JavaScript. The target is taken apart
//! by a declaration of hidden variables, which then are assigned to the places the target
//! names, in order:
//!
//! ```text
//! { const [<d0>, { y: <d1>, z: <d2> = undefined }] = value;
//!   a = <d0>; o.y = <d1>; z = #undefined_or(<d2>, 0); }
//! ```
//!
//! so the rest and the checks are those of declarations (`pattern_defaults`, `object_rest`).
//! A place with a default is bound as `T | undefined` (a default of `undefined`), and the
//! default is applied when its place is assigned, `place = #undefined_or(<dN>, default)`: as
//! in JavaScript, a default sees the places assigned before it and runs after the targets
//! before it are evaluated (`[a, b = a] = [5]` gives `b` 5). A nested pattern with a default
//! (`[{ x } = d] = xs`) is taken apart from its hidden variable when its turn comes. The
//! assignment has no value: used as one, it is an error (`value_use_message`). A hole
//! (`[a, , b] = xs`) is an element the pattern skips.
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

/// One place a destructuring assignment writes, in order (module docs).
struct Step {
    /// The hidden variable holding the value for the place.
    name: String,
    /// The place (a variable, field or element), or a nested pattern with a default, which is
    /// taken apart from the hidden variable when its turn comes.
    place: ast::Expr,
    /// The default, applied when the hidden variable is `undefined`.
    default: Option<ast::Expr>,
}

/// A target turned into a pattern: the pattern, and the places its hidden variables go to.
struct Split {
    steps: Vec<Step>,
    at: u32,
}

impl Split {
    fn hidden(&mut self, place: &ast::Expr, default: Option<&ast::Expr>) -> ast::Pattern {
        let name = format!("<destructured {} {}>", self.at, self.steps.len());
        let span = place.span;
        self.steps.push(Step {
            name: name.clone(),
            place: place.clone(),
            default: default.cloned(),
        });
        let id = pattern_of(ast::PatternKind::Ident(ident(&name, span)), span);
        match default {
            // Bound as `T | undefined` (a default of `undefined`); the real default is applied
            // when the place is assigned, after the places before it.
            Some(d) => {
                let undefined = expr_of(ast::ExprKind::Lit(ast::Lit::Undefined), d.span);
                pattern_of(
                    ast::PatternKind::Default {
                        pattern: Box::new(id),
                        value: Box::new(undefined),
                    },
                    span,
                )
            }
            None => id,
        }
    }

    /// The pattern for target `t`, or `None` for a target no pattern can express.
    fn pattern(&mut self, t: &ast::Expr) -> Option<ast::Pattern> {
        use ast::ExprKind as E;
        let kind = match &t.kind {
            E::Paren(inner) => return self.pattern(inner),
            E::Ident(_) | E::Member { .. } | E::Index { .. } => return Some(self.hidden(t, None)),
            E::Assign {
                op: None,
                target,
                value,
            } => {
                let mut inner = &**target;
                while let E::Paren(x) = &inner.kind {
                    inner = x;
                }
                return match &inner.kind {
                    E::Ident(_)
                    | E::Member { .. }
                    | E::Index { .. }
                    | E::Array(_)
                    | E::Object(_) => Some(self.hidden(inner, Some(value))),
                    _ => None,
                };
            }
            E::Array(items) => {
                let mut elems = vec![];
                let mut rest = None;
                for (i, x) in items.iter().enumerate() {
                    match &x.kind {
                        E::Spread(r) if i + 1 == items.len() => {
                            rest = Some(self.rest(r)?);
                        }
                        _ if is_hole(x) => {
                            elems.push(pattern_of(ast::PatternKind::Wildcard, x.span))
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
                            let place = expr_of(E::Ident(k.clone()), k.span);
                            fields.push((k.clone(), self.hidden(&place, Some(value))));
                        }
                        ast::ObjectProp::KeyValue(k, v) => {
                            fields.push((k.clone(), self.pattern(v)?))
                        }
                        ast::ObjectProp::Shorthand(k) => {
                            let place = expr_of(E::Ident(k.clone()), k.span);
                            fields.push((k.clone(), self.hidden(&place, None)));
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

/// The error for an assignment to `target` used as a value (`assign_value`): a destructuring
/// assignment's value is not supported (`x = [a, b] = c`), nor one of an element or a computed
/// place.
pub(super) fn value_use_message(target: &ast::Expr) -> &'static str {
    let mut t = target;
    while let ast::ExprKind::Paren(x) = &t.kind {
        t = x;
    }
    match t.kind {
        ast::ExprKind::Array(_) | ast::ExprKind::Object(_) => {
            "a destructuring assignment cannot be used as a value yet; assign the value to a variable first (`const v = c; [a, b] = v;`)"
        }
        _ => "an assignment to an element or a computed place cannot be used as a value",
    }
}

/// Is `x` a hole of an array target (`[a, , b]`)?
fn is_hole(x: &ast::Expr) -> bool {
    matches!(&x.kind, ast::ExprKind::Ident(i) if i.name == velt_syntax::ARRAY_HOLE)
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
            steps: vec![],
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
        let decl = ast::Stmt {
            kind: ast::StmtKind::Var(ast::VarDecl {
                kind: ast::VarKind::Const,
                pattern,
                ty: None,
                init: Some(value.clone()),
                span,
            }),
            span,
        };
        let mut out = vec![];
        self.push_scope();
        self.stmt(&decl, &mut out);
        for step in &split.steps {
            // The pattern did not take the value apart (reported): nothing to assign.
            if !self.declares_here(&step.name) {
                continue;
            }
            let at = step.place.span;
            let mut read = expr_of(ast::ExprKind::Ident(ident(&step.name, at)), at);
            // A default where the value is never `undefined` (a tuple's element, a field that is
            // not optional) never applies, as in a declaration.
            let default = step.default.as_ref().filter(|_| {
                let l = self.lookup_local(&step.name, at);
                l.is_some_and(|l| {
                    let t = self.f.locals[l.0 as usize].ty;
                    self.cx.dynamic(t).is_some()
                        || self.cx.ty.nothing_of(t).is_some_and(|n| n.has_undefined())
                })
            });
            if let Some(d) = default {
                let callee = ast::ExprKind::Ident(ident(crate::body::UNDEFINED_OR, at));
                read = expr_of(
                    ast::ExprKind::Call {
                        callee: Box::new(expr_of(callee, at)),
                        type_args: vec![],
                        args: vec![read, d.clone()],
                        optional: false,
                    },
                    at,
                );
            }
            let assign = ast::ExprKind::Assign {
                op: None,
                target: Box::new(step.place.clone()),
                value: Box::new(read),
            };
            let s = ast::Stmt {
                kind: ast::StmtKind::Expr(expr_of(assign, at)),
                span: at,
            };
            self.stmt(&s, &mut out);
        }
        self.pop_scope();
        let block = hir::Block {
            stmts: out,
            value: None,
            span,
        };
        Some(self.mk(H::Block(block), self.cx.ty.unit, span))
    }

    /// `{ a = 1 }` outside an assignment pattern (module docs): TypeScript's TS1312. The
    /// properties with each such one as `a: <error>`, so nothing more is reported for it; `None`
    /// when there is none.
    pub(super) fn reject_cover_inits(
        &mut self,
        props: &[ast::ObjectProp],
    ) -> Option<Vec<ast::ObjectProp>> {
        if !props.iter().any(|p| cover_init(p).is_some()) {
            return None;
        }
        let mut out = vec![];
        for p in props {
            match cover_init(p) {
                Some((k, _)) => {
                    self.cx.err(
                        "`=` after a shorthand property is only allowed in a destructuring assignment (`({ a = 1 } = o)`); did you mean `:`? (TypeScript's TS1312)",
                        k.span,
                    );
                    let err = expr_of(ast::ExprKind::Ident(ident(REPORTED, k.span)), k.span);
                    out.push(ast::ObjectProp::KeyValue(k.clone(), err));
                }
                None => out.push(p.clone()),
            }
        }
        Some(out)
    }
}

/// The name of an expression already reported, checked as an error without a message (no
/// source can write it).
pub(super) const REPORTED: &str = "#reported";
