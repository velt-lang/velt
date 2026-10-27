//! The chains a variable key of `globalThis` becomes (see the parent module): when the asserted
//! type lists its keys, a read, an assignment, a definition or a `delete` with `k` a variable is
//! a test of `k` against each key, the first one checking `k`'s type
//! (`body::expr::checked_key`).

use velt_syntax::ast::{self, ExprKind as E, StmtKind as S};

use super::{ident, synth, Walk, GLOBAL_THIS};
use crate::global_this::keys::listed_keys;

impl Walk<'_> {
    /// The variable `index` names and the keys the asserted type `t` lists, when `index` is a
    /// variable (not a string constant) and `t` lists a fixed set of keys.
    pub(super) fn listed(
        &self,
        t: &ast::TypeExpr,
        index: &ast::Expr,
    ) -> Option<(ast::Ident, Vec<String>)> {
        let E::Ident(k) = &index.kind else {
            return None;
        };
        if matches!(self.lookup(&k.name), Some(Some(_))) {
            return None;
        }
        Some((k.clone(), listed_keys(t, self.aliases)?))
    }

    /// `Object.defineProperty(g, k, d);` with `g` standing for `globalThis` as a type listing a
    /// fixed set of keys and `k` a variable: `if (k === "a") Object.defineProperty(g, "a", d);
    /// else if ...`, each branch rewritten as a definition of its key.
    pub(super) fn define_listed(&mut self, e: &ast::Expr) -> Option<S> {
        let E::Call { callee, args, .. } = &e.kind else {
            return None;
        };
        let E::Member { object, prop, .. } = &callee.kind else {
            return None;
        };
        let is_object = matches!(&object.kind, E::Ident(o) if o.name == "Object" && self.lookup("Object").is_none());
        if !is_object
            || prop.name != "defineProperty"
            || args.len() != 3
            || self.key(&args[1]).is_some()
        {
            return None;
        }
        let Some(Some(t)) = self.base(&args[0]) else {
            return None;
        };
        let (k, keys) = self.listed(&t, &args[1])?;
        let span = e.span;
        let mut els: Option<Box<ast::Stmt>> = None;
        let all = keys.clone();
        for (i, key) in keys.into_iter().enumerate().rev() {
            let mut call = e.clone();
            if let E::Call { args, .. } = &mut call.kind {
                args[1] = synth(E::Lit(ast::Lit::Str(key.clone())), args[1].span);
            }
            self.define_property(&mut call, true);
            let then = ast::Block {
                stmts: vec![ast::Stmt {
                    kind: S::Expr(call),
                    span,
                }],
                span,
            };
            els = Some(Box::new(ast::Stmt {
                kind: S::If {
                    cond: is_key(&k, &key, (i == 0).then_some((&all[..], &t, true)), span),
                    then,
                    els,
                },
                span,
            }));
        }
        els.map(|s| s.kind)
    }

    /// `g[k] = v;` (or `op=`) with `g` standing for `globalThis` as a type listing a fixed set
    /// of keys and `k` a variable: `if (k === "a") g.a = v; else if ...`, each branch rewritten
    /// as an assignment of its key.
    pub(super) fn assign_listed(&mut self, e: &ast::Expr) -> Option<S> {
        let target = match &e.kind {
            E::Assign { target, .. } | E::Update { target, .. } => target,
            _ => return None,
        };
        // `h[k]!++`: the assertion is kept for the branches (`h.a = h.a! + 1`).
        let mut target = &**target;
        let mut asserted = false;
        while let E::NonNull(x) | E::Paren(x) = &target.kind {
            asserted |= matches!(target.kind, E::NonNull(_));
            target = x;
        }
        let E::Index { object, index, .. } = &target.kind else {
            return None;
        };
        if self.key(index).is_some() {
            return None;
        }
        let Some(Some(t)) = self.base(object) else {
            return None;
        };
        let (k, keys) = self.listed(&t, index)?;
        let span = e.span;
        let mut els: Option<Box<ast::Stmt>> = None;
        let all = keys.clone();
        for (i, key) in keys.into_iter().enumerate().rev() {
            let member = synth(
                E::Member {
                    object: Box::new(synth(
                        E::Paren(Box::new(synth(
                            E::Cast {
                                expr: Box::new(ident(GLOBAL_THIS, span)),
                                ty: t.clone(),
                            },
                            span,
                        ))),
                        span,
                    )),
                    prop: ast::Ident {
                        name: key.clone(),
                        span: target.span,
                    },
                    optional: false,
                },
                target.span,
            );
            let kind = match &e.kind {
                E::Assign { op, value, .. } => E::Assign {
                    op: *op,
                    target: Box::new(member),
                    value: value.clone(),
                },
                // A statement: its value is unused, so `x!++` is `x = x! + 1`.
                E::Update { op, .. } if asserted => {
                    let one = synth(
                        E::Lit(ast::Lit::Int {
                            value: 1,
                            suffix: None,
                        }),
                        span,
                    );
                    let op = match op {
                        ast::UpdateOp::Inc => ast::BinaryOp::Add,
                        ast::UpdateOp::Dec => ast::BinaryOp::Sub,
                    };
                    E::Assign {
                        op: None,
                        target: Box::new(member.clone()),
                        value: Box::new(synth(
                            E::Binary {
                                op,
                                lhs: Box::new(synth(E::NonNull(Box::new(member)), span)),
                                rhs: Box::new(one),
                            },
                            span,
                        )),
                    }
                }
                E::Update { op, prefix, .. } => E::Update {
                    op: *op,
                    prefix: *prefix,
                    target: Box::new(member),
                },
                _ => return None,
            };
            let mut assign = synth(kind, span);
            self.expr(&mut assign, true);
            let then = ast::Block {
                stmts: vec![ast::Stmt {
                    kind: S::Expr(assign),
                    span,
                }],
                span,
            };
            els = Some(Box::new(ast::Stmt {
                kind: S::If {
                    cond: is_key(&k, &key, (i == 0).then_some((&all[..], &t, false)), span),
                    then,
                    els,
                },
                span,
            }));
        }
        els.map(|s| s.kind)
    }

    /// `delete (globalThis as T)[k]` (or `.K`, `[KEY]`): `globalThis.deleteKey<keys, T>(k)`,
    /// which checks the key as a read does and reports that deleting is not supported yet.
    pub(super) fn delete_property(&mut self, e: &mut ast::Expr) -> bool {
        let E::Unary {
            op: ast::UnaryOp::Delete,
            expr,
        } = &e.kind
        else {
            return false;
        };
        let mut place = &**expr;
        while let E::Paren(x) | E::NonNull(x) = &place.kind {
            place = x;
        }
        let (object, key) = match &place.kind {
            E::Member { object, prop, .. } => {
                let k = synth(E::Lit(ast::Lit::Str(prop.name.clone())), prop.span);
                (object, k)
            }
            E::Index { object, index, .. } => match self.key(index) {
                Some(k) => (object, synth(E::Lit(ast::Lit::Str(k)), index.span)),
                None => (object, (**index).clone()),
            },
            _ => return false,
        };
        let Some(Some(t)) = self.base(object) else {
            return false;
        };
        let keys = match (&key.kind, listed_keys(&t, self.aliases)) {
            (_, Some(ks)) => ks,
            (E::Lit(ast::Lit::Str(k)), None) => vec![k.clone()],
            _ => return false,
        };
        let span = e.span;
        let lit = |s: &String| ast::TypeExpr {
            kind: ast::TypeExprKind::Literal(ast::SignedLit {
                lit: ast::Lit::Str(s.clone()),
                negative: false,
            }),
            span,
        };
        let union = match &keys[..] {
            [one] => lit(one),
            _ => ast::TypeExpr {
                kind: ast::TypeExprKind::Union(keys.iter().map(lit).collect()),
                span,
            },
        };
        let mut key = key;
        self.expr(&mut key, false);
        *e = synth(
            E::Call {
                callee: Box::new(ident(DELETE_KEY_CHECK, span)),
                type_args: vec![union, t],
                args: vec![key],
                optional: false,
            },
            span,
        );
        true
    }
}

/// The checks of a variable key against the listed keys (`body::expr::checked_key`): a read
/// or an assignment, and a definition.
pub(crate) const KEY_CHECK: &str = "globalThis.key";
pub(crate) const DEFINE_KEY_CHECK: &str = "globalThis.defineKey";
pub(crate) const DELETE_KEY_CHECK: &str = "globalThis.deleteKey";

/// The first test of a chain checks the key: the listed keys, the asserted type (for the
/// message) and whether the site is a `defineProperty`.
type Listed<'a> = Option<(&'a [String], &'a ast::TypeExpr, bool)>;

pub(super) fn is_key(
    k: &ast::Ident,
    key: &str,
    listed: Listed<'_>,
    span: velt_common::Span,
) -> ast::Expr {
    let string = ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ast::Ident {
                name: "string".into(),
                span: k.span,
            }],
            args: vec![],
        },
        span: k.span,
    };
    let mut read = synth(E::Ident(k.clone()), k.span);
    if let Some((keys, asserted, define)) = listed {
        let lit = |s: &String| ast::TypeExpr {
            kind: ast::TypeExprKind::Literal(ast::SignedLit {
                lit: ast::Lit::Str(s.clone()),
                negative: false,
            }),
            span: k.span,
        };
        let union = match keys {
            [one] => lit(one),
            _ => ast::TypeExpr {
                kind: ast::TypeExprKind::Union(keys.iter().map(lit).collect()),
                span: k.span,
            },
        };
        let check = if define { DEFINE_KEY_CHECK } else { KEY_CHECK };
        read = synth(
            E::Call {
                callee: Box::new(ident(check, k.span)),
                type_args: vec![union, asserted.clone()],
                args: vec![read],
                optional: false,
            },
            span,
        );
    }
    let k = synth(
        E::Cast {
            expr: Box::new(read),
            ty: string,
        },
        k.span,
    );
    synth(
        E::Binary {
            op: ast::BinaryOp::StrictEq,
            lhs: Box::new(k),
            rhs: Box::new(synth(E::Lit(ast::Lit::Str(key.to_string())), span)),
        },
        span,
    )
}
