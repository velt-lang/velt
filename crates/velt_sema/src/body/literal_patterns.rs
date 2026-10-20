//! An array pattern taking apart an array literal (`const [a, s] = [1, "s"]`, and the hidden
//! declaration of a destructuring assignment, `[x, s] = [x + 1, s + "!"]`): as in TypeScript,
//! the pattern types the literal as a tuple, so each element keeps its own type instead of the
//! union of all of them. The elements are evaluated in order into hidden temporaries, then each
//! is bound to its pattern (with its default), as JavaScript evaluates the whole literal before
//! taking it apart:
//!
//! ```text
//! const [a, b = d] = [e1, e2];   →   const #d1 = e1; const #d2 = e2; const a = #d1; const b = #d2 ?? d;
//! ```
//!
//! Only a literal without spreads or holes, at least as long as the pattern, and a pattern
//! without a rest; `[…] as const` counts as the literal.
//!
//! A pattern taking apart an `any` (`const [a, b] = v`, `const { p, q = 1 } = v`) reads each
//! element or property through the `any` (`v[0]`, `v.p`), each an `any`, as TypeScript types
//! them; a default applies where the read gives `undefined` (`v.q === undefined ? 1 : v.q`).
//!
//! An object pattern with a computed key naming a local variable (`const { [k]: v } = r`, the
//! parser's `[k]` key) reads that property as `r[k]` the same way (a `[KEY]` naming a module
//! constant is a field or symbol key, resolved where the pattern is checked).

use velt_syntax::ast::{self, Expr, ExprKind as E, PatternKind as P, VarDecl, VarKind};

use super::pattern_defaults::{ident, mk, pat};
use super::FnCx;
use crate::hir;

/// The elements of the array literal `e` (in parentheses, or `as const`), if it has neither a
/// spread nor a hole.
fn literal_elems(e: &Expr) -> Option<&[Expr]> {
    match &e.kind {
        E::Paren(inner) => literal_elems(inner),
        E::Cast { expr, ty } if super::expr::member::is_as_const(ty) => literal_elems(expr),
        E::Array(items)
            if !items.iter().any(|x| {
                matches!(&x.kind, E::Spread(_))
                    || matches!(&x.kind, E::Ident(i) if i.name == velt_syntax::ARRAY_HOLE)
            }) =>
        {
            Some(items)
        }
        _ => None,
    }
}

impl FnCx<'_, '_> {
    /// Evaluate the elements `items` into hidden temporaries, in order, and list each pattern
    /// of `elems` with the temporary it takes apart (`binds`). A nested array pattern without a
    /// default or rest over a nested literal is taken apart the same way, so its elements keep
    /// their types too.
    fn literal_temps(
        &mut self,
        elems: &[ast::Pattern],
        items: &[Expr],
        binds: &mut Vec<(ast::Pattern, Expr)>,
        out: &mut Vec<hir::Stmt>,
    ) {
        for (k, x) in items.iter().enumerate() {
            let p = elems.get(k);
            if let Some(ast::Pattern {
                kind:
                    P::Array {
                        elems: inner,
                        rest: None,
                    },
                ..
            }) = p
            {
                if let Some(nested) = literal_elems(x).filter(|n| n.len() >= inner.len()) {
                    self.literal_temps(inner, nested, binds, out);
                    continue;
                }
            }
            let span = x.span;
            let name = ident(&format!("#d{}", self.f.locals.len()), span);
            let d = VarDecl {
                kind: VarKind::Const,
                pattern: pat(P::Ident(name.clone()), span),
                ty: None,
                init: Some(x.clone()),
                span,
            };
            self.var_decl(&d, span, out);
            if let Some(p) = p.filter(|p| !matches!(p.kind, P::Wildcard)) {
                binds.push((p.clone(), mk(E::Ident(name), span)));
            }
        }
    }

    /// `kind [p1, p2] = [e1, e2, …]` without a type annotation, element by element (module
    /// docs); `false` (nothing done) for any other declaration.
    pub(super) fn literal_pattern_decl(&mut self, v: &VarDecl, out: &mut Vec<hir::Stmt>) -> bool {
        let (P::Array { elems, rest: None }, None, Some(init)) = (&v.pattern.kind, &v.ty, &v.init)
        else {
            return false;
        };
        let Some(items) = literal_elems(init) else {
            return false;
        };
        if items.len() < elems.len() {
            return false;
        }
        let mut binds = vec![];
        self.literal_temps(elems, items, &mut binds, out);
        for (p, t) in binds {
            self.decl_with_defaults(v.kind, &p, t, out);
        }
        true
    }
}

impl FnCx<'_, '_> {
    /// `kind pattern = tmp` where the hidden temporary (or variable) `tmp` holds an `any`
    /// (module docs).
    pub(super) fn any_pattern_decls(
        &mut self,
        kind: VarKind,
        pattern: &ast::Pattern,
        tmp: &Expr,
        out: &mut Vec<hir::Stmt>,
    ) {
        self.read_pattern_decls(kind, pattern, tmp, true, out);
    }

    /// `kind pattern = tmp`, each element or property read from `tmp` (an index, a member, or
    /// `tmp[k]` for a computed key); `dynamic`: `tmp` is an `any` (module docs).
    fn read_pattern_decls(
        &mut self,
        kind: VarKind,
        pattern: &ast::Pattern,
        tmp: &Expr,
        dynamic: bool,
        out: &mut Vec<hir::Stmt>,
    ) {
        let reads: Vec<(&ast::Pattern, Expr)> = match &pattern.kind {
            P::Array { elems, rest } => {
                if let Some(r) = rest {
                    self.cx.err(
                        "`...rest` of an `any` value is not supported yet; give the value an array type first",
                        r.span,
                    );
                }
                elems
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| !matches!(p.kind, P::Wildcard))
                    .map(|(k, p)| {
                        let at = mk(
                            E::Lit(ast::Lit::Int {
                                value: k as u128,
                                suffix: None,
                            }),
                            p.span,
                        );
                        let read = E::Index {
                            object: Box::new(tmp.clone()),
                            index: Box::new(at),
                            optional: false,
                        };
                        (p, mk(read, p.span))
                    })
                    .collect()
            }
            P::Object { fields, rest } => {
                if let Some(r) = rest {
                    let msg = match dynamic {
                        true => "`...rest` of an `any` value is not supported yet; give the value an object type first",
                        false => "`...rest` next to a computed key known only at run time is not supported yet",
                    };
                    self.cx.err(msg, r.span);
                }
                fields
                    .iter()
                    .map(|(key, p)| {
                        let read = match self.computed_local_key(key) {
                            Some(k) => E::Index {
                                object: Box::new(tmp.clone()),
                                index: Box::new(mk(E::Ident(k), key.span)),
                                optional: false,
                            },
                            None => E::Member {
                                object: Box::new(tmp.clone()),
                                prop: key.clone(),
                                optional: false,
                            },
                        };
                        (p, mk(read, key.span))
                    })
                    .collect()
            }
            _ => return,
        };
        for (p, read) in reads {
            let (p, value) = match &p.kind {
                P::Default { pattern, value } if !dynamic => {
                    let span = read.span;
                    let callee = mk(E::Ident(ident(crate::body::UNDEFINED_OR, span)), span);
                    let call = E::Call {
                        callee: Box::new(callee),
                        type_args: vec![],
                        args: vec![read, (**value).clone()],
                        optional: false,
                    };
                    (&**pattern, mk(call, span))
                }
                P::Default { pattern, value }
                    if !matches!(value.kind, E::Lit(ast::Lit::Undefined)) =>
                {
                    let span = read.span;
                    let test = mk(
                        E::Binary {
                            op: ast::BinaryOp::StrictEq,
                            lhs: Box::new(read.clone()),
                            rhs: Box::new(mk(E::Lit(ast::Lit::Undefined), span)),
                        },
                        span,
                    );
                    let d = mk(
                        E::Cast {
                            expr: value.clone(),
                            ty: ast::TypeExpr {
                                kind: ast::TypeExprKind::Named {
                                    path: vec![ident("any", span)],
                                    args: vec![],
                                },
                                span,
                            },
                        },
                        span,
                    );
                    let cond = E::Cond {
                        cond: Box::new(test),
                        then: Box::new(d),
                        els: Box::new(read),
                    };
                    (&**pattern, mk(cond, span))
                }
                P::Default { pattern, .. } => (&**pattern, read),
                _ => (p, read),
            };
            self.decl_with_defaults(kind, p, value, out);
        }
    }
}

impl FnCx<'_, '_> {
    /// The variable a computed key `[k]` of an object pattern names, when it is a local (not a
    /// module constant, which names a field or a symbol).
    fn computed_local_key(&mut self, key: &ast::Ident) -> Option<ast::Ident> {
        let inner = key.name.strip_prefix('[')?.strip_suffix(']')?;
        if inner.starts_with("Symbol.") || !self.declares_here(inner) && !self.outer_names(inner) {
            return None;
        }
        Some(ident(inner, key.span))
    }

    /// `kind { [k]: v, … } = init` with a computed key naming a local (module docs); `false`
    /// (nothing done) for any other declaration.
    pub(super) fn computed_key_decl(&mut self, v: &VarDecl, out: &mut Vec<hir::Stmt>) -> bool {
        let (P::Object { fields, .. }, Some(init)) = (&v.pattern.kind, &v.init) else {
            return false;
        };
        if !fields
            .iter()
            .any(|(k, _)| self.computed_local_key(k).is_some())
        {
            return false;
        }
        let span = init.span;
        let name = ident(&format!("#d{}", self.f.locals.len()), span);
        let d = VarDecl {
            kind: VarKind::Const,
            pattern: pat(P::Ident(name.clone()), span),
            ty: v.ty.clone(),
            init: Some(init.clone()),
            span,
        };
        self.var_decl(&d, span, out);
        let tmp = mk(E::Ident(name), span);
        self.read_pattern_decls(v.kind, &v.pattern, &tmp, false, out);
        true
    }
}
