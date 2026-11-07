//! Rest parameters of generic function types that a call's type arguments decide
//! (`...args: Detail<E, N> extends void ? [] : [Detail<E, N>]`, `crate::generic_fns`). The method
//! takes an `any[]`, the same at every instantiation. A call is checked against the form at its
//! type arguments: a tuple element by element, a union by the member with this many arguments, an
//! array by its element type. A function of the type sees JavaScript's arguments: `...args` is
//! the array, and a parameter written at the rest position is its element there.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::{Callable, Checked};
use crate::body::{FnCx, Want};
use crate::generic_fns::GenericSig;
use crate::hir::{ExprKind as H, TyId, TyKind};

/// `name.at(k) as any`: a rest argument of a function of the type, `undefined` past the end.
pub(super) fn rest_item(name: &ast::Ident, k: usize) -> ast::Expr {
    let span = name.span;
    let mk = |kind| ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    };
    let member = mk(ast::ExprKind::Member {
        object: Box::new(mk(ast::ExprKind::Ident(name.clone()))),
        prop: ast::Ident {
            name: "at".into(),
            span,
        },
        optional: false,
    });
    let at = mk(ast::ExprKind::Lit(ast::Lit::Int {
        value: k as u128,
        suffix: None,
    }));
    let read = mk(ast::ExprKind::Call {
        callee: Box::new(member),
        type_args: vec![],
        args: vec![at],
        optional: false,
    });
    let any = ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ast::Ident {
                name: "any".into(),
                span,
            }],
            args: vec![],
        },
        span,
    };
    mk(ast::ExprKind::Cast {
        expr: Box::new(read),
        ty: any,
    })
}

/// How many arguments rest literal `arg` holds, and whether a spread is among them.
fn rest_count(arg: &ast::Expr) -> (usize, bool) {
    match &super::object_builtins::strip_parens(arg).kind {
        ast::ExprKind::Array(xs) => (
            xs.len(),
            xs.iter()
                .any(|x| matches!(x.kind, ast::ExprKind::Spread(_))),
        ),
        _ => (0, false),
    }
}

impl FnCx<'_, '_> {
    /// `t`, the type parameter `i` of call `c` is expected to have with arguments `arg`: for a
    /// rest parameter decided by the type arguments, what the literal of its arguments is checked
    /// as ([`Self::rest_member`]).
    pub(super) fn rest_form_target(
        &mut self,
        c: &Callable,
        i: usize,
        t: TyId,
        arg: &ast::Expr,
    ) -> TyId {
        if !c.rest || i + 1 != c.params.len() || !self.cx.is_rest_form(c.params[i].ty) {
            return t;
        }
        let (n, spread) = rest_count(arg);
        self.rest_member(t, n, spread)
    }

    /// The type a literal of `n` rest arguments is checked as, form `t` being at the call's type
    /// arguments: the tuple itself, a union's member with `n` elements or its array, an `any[]`
    /// for an array (its elements are checked in [`Self::rest_form_args`]), `any`, or a form the
    /// enclosing function's type parameters still decide. `t` when nothing fits.
    fn rest_member(&mut self, t: TyId, n: usize, spread: bool) -> TyId {
        let Some(any) = self.any_ty() else {
            return t;
        };
        if self.cx.ty.has_error(t) {
            return t;
        }
        let array = self.cx.ty.array(any);
        if self.cx.has_type_params(t) || self.cx.dynamic(t).is_some() {
            return array;
        }
        match self.cx.ty.kind(t) {
            TyKind::Array(_) => return array,
            TyKind::Tuple(_) => return t,
            _ => {}
        }
        for m in self.cx.union_members(t).unwrap_or_default() {
            match self.cx.ty.kind(m) {
                TyKind::Tuple(ts) if ts.len() == n && !spread => return m,
                TyKind::Array(_) => return array,
                _ => {}
            }
        }
        t
    }

    /// The checked arguments `ck` of a call whose rest parameter is `form` at the call's type
    /// arguments (module docs), the rest arguments as the `any[]` the method takes.
    pub(super) fn rest_form_args(
        &mut self,
        c: &Callable,
        form: TyId,
        ck: &mut Checked,
        args: &[ast::Expr],
        span: Span,
    ) {
        let at = c.params.len() - 1;
        if ck.args.len() != c.params.len() {
            return; // Reported.
        }
        let want = self.cx.subst(form, &ck.type_args);
        if self.cx.ty.has_error(want) {
            return;
        }
        let rest = args.get(at..).unwrap_or_default();
        let spread = rest
            .iter()
            .any(|x| matches!(x.kind, ast::ExprKind::Spread(_)));
        let member = self.rest_member(want, rest.len(), spread);
        if self.cx.has_type_params(want) {
            if rest
                .iter()
                .any(|x| !matches!(x.kind, ast::ExprKind::Spread(_)))
            {
                let shown = self.cx.display(want);
                self.cx.error(
                    Diagnostic::error(
                        format!("arguments for a rest parameter of type `{shown}`, which the type parameters of this function still decide, are not supported yet (#943)"),
                        span,
                    )
                    .with_note("pass on the rest parameter of the same form: `f(n, ...args)`"),
                );
            }
            return;
        }
        if self.cx.union_members(want).is_some() && member == want {
            let shown = self.cx.display(want);
            self.cx.err(
                format!(
                    "these {} arguments match no member of the rest parameter's type `{shown}`",
                    rest.len()
                ),
                span,
            );
            return;
        }
        match self.cx.ty.kind(want).clone() {
            TyKind::Tuple(ts) if rest.is_empty() && !ts.is_empty() => {
                self.arg_count_error(&c.what, at + ts.len(), at + ts.len(), args.len(), span);
                return;
            }
            TyKind::Array(e) => self.check_rest_items(rest, e),
            _ if matches!(self.cx.ty.kind(member), TyKind::Tuple(_) | TyKind::Array(_)) => {}
            _ if self.cx.dynamic(want).is_some() => {}
            _ => {
                let shown = self.cx.display(want);
                self.cx.err(
                    format!("a rest parameter must have an array or tuple type; at this call it is `{shown}`"),
                    span,
                );
                return;
            }
        }
        if rest.is_empty() {
            return; // The empty array is the parameter's default.
        }
        let (Some(any), Some(last)) = (self.any_ty(), ck.args.pop()) else {
            return;
        };
        let ty = self.cx.ty.array(any);
        let span = last.span;
        let value = match last.kind {
            H::Tuple(items) => {
                let items = items.into_iter().map(|e| self.coerce(e, any)).collect();
                self.mk(H::ArrayLit(items), ty, span)
            }
            _ => self.coerce(last, ty),
        };
        ck.args.push(value);
    }

    /// The rest arguments `rest` against an array form's element type `e`, reported as they
    /// would be for a rest parameter of that type; their values are in the `any[]`.
    fn check_rest_items(&mut self, rest: &[ast::Expr], e: TyId) {
        let array = self.cx.ty.array(e);
        for x in rest {
            let (item, t) = match &x.kind {
                ast::ExprKind::Spread(inner) => (&**inner, array),
                _ => (x, e),
            };
            let fits = self
                .trial_with(|s| s.expr_coerce(item, t, Want::Move).ty)
                .is_some();
            if !fits {
                self.expr_coerce(item, t, Want::Move);
            }
        }
    }

    /// Arrow `e` as a function of generic function type `g`, whose rest parameter a call's type
    /// arguments decide, when it writes parameters at the rest position: `(n, d) => body` as
    /// `(n, ...r) => { let d = r.at(0) as any; body }`, what JavaScript binds. `None` for any
    /// other arrow.
    pub(super) fn rest_items_arrow(&mut self, e: &ast::Expr, g: &GenericSig) -> Option<ast::Expr> {
        let ast::ExprKind::Arrow {
            type_params,
            params,
            ret,
            throws,
            body,
            is_async,
        } = &e.kind
        else {
            return None;
        };
        self.cx.generic_fn_rest_form(g.iface)?;
        let at = g.method.params.len().checked_sub(1)?;
        let written = params.get(at..)?;
        if written.is_empty() || written.iter().any(|p| p.rest) {
            return None;
        }
        let span = written[0].name.span;
        let rest = ast::Ident {
            name: "#rest".into(),
            span,
        };
        let mut stmts = vec![];
        for (k, p) in written.iter().enumerate() {
            let item = rest_item(&rest, k);
            let init = match p.default.as_ref().filter(|_| !p.optional) {
                Some(d) => ast::Expr {
                    id: ast::NodeId(u32::MAX),
                    kind: ast::ExprKind::Cond {
                        cond: Box::new(crate::body::defaults::undefined_test(item.clone(), span)),
                        then: Box::new(d.clone()),
                        els: Box::new(item),
                    },
                    span,
                },
                None => item,
            };
            let pattern = p.pattern.clone().unwrap_or_else(|| ast::Pattern {
                id: ast::NodeId(u32::MAX),
                kind: ast::PatternKind::Ident(p.name.clone()),
                span,
            });
            stmts.push(ast::Stmt {
                kind: ast::StmtKind::Var(ast::VarDecl {
                    kind: ast::VarKind::Let,
                    pattern,
                    ty: p.ty.clone().filter(|_| !p.optional),
                    init: Some(init),
                    span,
                    c_const: false,
                }),
                span,
            });
        }
        let block = match body {
            ast::ArrowBody::Expr(b) => {
                stmts.push(ast::Stmt {
                    kind: ast::StmtKind::Return(Some((**b).clone())),
                    span: b.span,
                });
                ast::Block {
                    stmts,
                    span: b.span,
                }
            }
            ast::ArrowBody::Block(b) => {
                stmts.extend(b.stmts.iter().cloned());
                ast::Block {
                    stmts,
                    span: b.span,
                }
            }
        };
        let mut ps: Vec<ast::ArrowParam> = params[..at].to_vec();
        ps.push(ast::ArrowParam {
            name: rest,
            pattern: None,
            ty: None,
            default: None,
            optional: false,
            rest: true,
        });
        Some(ast::Expr {
            id: e.id,
            kind: ast::ExprKind::Arrow {
                type_params: type_params.clone(),
                params: ps,
                ret: ret.clone(),
                throws: throws.clone(),
                body: ast::ArrowBody::Block(block),
                is_async: *is_async,
            },
            span: e.span,
        })
    }
}
