//! Signature callbacks adapted to the implementation's (docs/reference/functions.md
//! "Overloads"). A signature's function parameter that takes fixed parameters where the
//! implementation's takes a rest parameter (`setup: (ctx: Ctx, p1: T1) => R` for `setup: (ctx:
//! Ctx, ...args: unknown[]) => R`) fits, as TypeScript relates the two loosely (bivariantly).
//! The signature's body (`crate::overloads`) passes an adapter in its place:
//!
//! ```text
//! (a#0, ...rest#) => setup(a#0, rest#.length > 0
//!     ? <overloadArg><T1>(rest#[0], "…called with a wrong argument 2")
//!     : <overloadArg><T1>(undefined as unknown, "…called without its argument 2 …"))
//! ```
//!
//! Its parameters take their types from the implementation's parameter (its type parameters
//! too, which the signature cannot name). An argument the implementation leaves out is
//! `undefined`, as in Node, when the callback's parameter type admits it; otherwise, and for an
//! argument of another type, the program stops with a `TypeError` naming the callback and the
//! argument (`body::expr::overload_arg`).
//!
//! The adapter is a new function for each call of the signature, so the implementation would
//! see another function than the caller passed if it compared or kept the callback (`===`, a
//! `Map` key, `includes`). [`check_identity`] makes any use of such a parameter other than a
//! call an error.

use rustc_hash::FxHashSet;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{self, Callee, Def, ExprKind as H, LocalId};
use crate::visit::{self, VisitMut};

/// The callee name of the checked conversion (`body::expr::overload_arg::OVERLOAD_ARG`).
const OVERLOAD_ARG: &str = "<overloadArg>";

/// An implementation's parameter that a signature passes an adapter for.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Adapted {
    /// The implementation's signature span and name span (one is its `FnDef::span`).
    pub imp: (Span, Span),
    pub name: String,
    pub param: usize,
    pub param_name: String,
}

/// The adapter for signature parameter `p` (of the overload set `name`) where the
/// implementation's `ip` takes a rest parameter (module docs), or `None` when `p` is passed
/// as it is.
pub(crate) fn rest_adapter(
    p: &ast::Param,
    ip: &ast::Param,
    name: &str,
    span: Span,
) -> Option<ast::Expr> {
    use ast::TypeExprKind as T;
    let (
        T::Function {
            params: want,
            rest: false,
            ..
        },
        T::Function {
            params: have,
            rest: true,
            ..
        },
    ) = (&p.ty.kind, &ip.ty.kind)
    else {
        return None;
    };
    if p.rest || ip.rest || p.optional || ip.optional {
        return None;
    }
    let (last, fixed) = have.split_last()?;
    let T::Array(_) = &last.kind else {
        return None;
    };
    if want.len() < fixed.len() {
        return None;
    }
    let expr = |kind| ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    };
    let ident = |n: &str| ast::Ident {
        name: n.to_string(),
        span,
    };
    let read = |n: &str| expr(ast::ExprKind::Ident(ident(n)));
    let param = |name: String, rest: bool| ast::ArrowParam {
        name: ident(&name),
        pattern: None,
        ty: None,
        default: None,
        optional: false,
        rest,
    };
    let mut params: Vec<ast::ArrowParam> = (0..fixed.len())
        .map(|i| param(format!("a#{i}"), false))
        .collect();
    params.push(param("rest#".into(), true));
    let cast = |e: ast::Expr, ty: &ast::TypeExpr| {
        expr(ast::ExprKind::Cast {
            expr: Box::new(e),
            ty: ty.clone(),
        })
    };
    let text = |s: String| expr(ast::ExprKind::Lit(ast::Lit::Str(s)));
    let int = |n: usize| {
        expr(ast::ExprKind::Lit(ast::Lit::Int {
            value: n as u128,
            suffix: None,
        }))
    };
    let checked = |v: ast::Expr, ty: &ast::TypeExpr, words: String| {
        expr(ast::ExprKind::Call {
            callee: Box::new(read(OVERLOAD_ARG)),
            type_args: vec![ty.clone()],
            args: vec![v, text(words)],
            optional: false,
        })
    };
    let cb = &p.name.name;
    let args = want
        .iter()
        .enumerate()
        .map(|(j, w)| {
            if j < fixed.len() {
                let a = read(&format!("a#{j}"));
                return match crate::overloads::same_type(&fixed[j], w) {
                    true => a,
                    false => cast(a, w),
                };
            }
            let k = j - fixed.len();
            let length = expr(ast::ExprKind::Member {
                object: Box::new(read("rest#")),
                prop: ident("length"),
                optional: false,
            });
            let present = expr(ast::ExprKind::Binary {
                op: ast::BinaryOp::Gt,
                lhs: Box::new(length),
                rhs: Box::new(int(k)),
            });
            let at = expr(ast::ExprKind::Index {
                object: Box::new(read("rest#")),
                index: Box::new(int(k)),
                optional: false,
            });
            let undefined = expr(ast::ExprKind::Lit(ast::Lit::Undefined));
            let unknown = ast::TypeExpr {
                kind: T::Named {
                    path: vec![ident("unknown")],
                    args: vec![],
                },
                span,
            };
            let n = j + 1;
            let wrong =
                format!("the callback `{cb}` of `{name}` was called with a wrong argument {n}");
            let missing = format!(
                "the callback `{cb}` of `{name}` was called without its argument {n} (Node would pass `undefined`)"
            );
            expr(ast::ExprKind::Cond {
                cond: Box::new(present),
                then: Box::new(checked(at, w, wrong)),
                els: Box::new(checked(cast(undefined, &unknown), w, missing)),
            })
        })
        .collect();
    let call = expr(ast::ExprKind::Call {
        callee: Box::new(read(cb)),
        type_args: vec![],
        args,
        optional: false,
    });
    Some(expr(ast::ExprKind::Arrow {
        type_params: vec![],
        params,
        ret: None,
        throws: None,
        body: ast::ArrowBody::Expr(Box::new(call)),
        is_async: false,
    }))
}

/// The arguments of a call of an overloaded function with each arrow parameter that has a
/// default and a written type (`(c: Ctx, p1: number = 5) => …`) taking `T | undefined` and
/// applying the default to `undefined`, as TypeScript types it (`p1?: number`): `(c: Ctx,
/// p1#in: number | undefined) => { let p1: number = p1#in === undefined ? 5 : p1#in; … }`. A
/// signature's adapted callback then passes `undefined` for an argument the implementation
/// leaves out, and the default applies, as in Node. `None` when no argument has one.
pub(crate) fn optional_defaults(args: &[ast::Expr]) -> Option<Vec<ast::Expr>> {
    let has = |e: &ast::Expr| {
        matches!(&e.kind, ast::ExprKind::Arrow { params, .. }
            if params.iter().any(defaulted))
    };
    if !args.iter().any(has) {
        return None;
    }
    Some(
        args.iter()
            .map(|a| match &a.kind {
                ast::ExprKind::Arrow {
                    type_params,
                    params,
                    ret,
                    throws,
                    body,
                    is_async,
                } if has(a) => {
                    let span = a.span;
                    let mut lets = vec![];
                    let params = params
                        .iter()
                        .map(|p| {
                            if !defaulted(p) {
                                return p.clone();
                            }
                            let (Some(ty), Some(d)) = (&p.ty, &p.default) else {
                                return p.clone();
                            };
                            let inner = format!("{}#in", p.name.name);
                            lets.push(default_let(&p.name, &inner, ty, d, span));
                            let undefined = ast::TypeExpr {
                                kind: ast::TypeExprKind::Undefined,
                                span,
                            };
                            ast::ArrowParam {
                                name: ast::Ident {
                                    name: inner,
                                    span: p.name.span,
                                },
                                ty: Some(ast::TypeExpr {
                                    kind: ast::TypeExprKind::Union(vec![ty.clone(), undefined]),
                                    span: ty.span,
                                }),
                                default: None,
                                ..p.clone()
                            }
                        })
                        .collect();
                    let mut stmts = lets;
                    match body {
                        ast::ArrowBody::Expr(e) => stmts.push(ast::Stmt {
                            kind: ast::StmtKind::Return(Some((**e).clone())),
                            span: e.span,
                        }),
                        ast::ArrowBody::Block(b) => stmts.extend(b.stmts.iter().cloned()),
                    }
                    ast::Expr {
                        id: a.id,
                        kind: ast::ExprKind::Arrow {
                            type_params: type_params.clone(),
                            params,
                            ret: ret.clone(),
                            throws: throws.clone(),
                            body: ast::ArrowBody::Block(ast::Block { stmts, span }),
                            is_async: *is_async,
                        },
                        span,
                    }
                }
                _ => a.clone(),
            })
            .collect(),
    )
}

/// A plain parameter with a written type and a default (not optional, rest or destructured).
fn defaulted(p: &ast::ArrowParam) -> bool {
    p.ty.is_some() && p.default.is_some() && !p.optional && !p.rest && p.pattern.is_none()
}

/// `let name: ty = inner === undefined ? d : inner;`
fn default_let(
    name: &ast::Ident,
    inner: &str,
    ty: &ast::TypeExpr,
    d: &ast::Expr,
    span: Span,
) -> ast::Stmt {
    let expr = |kind| ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    };
    let read = || {
        expr(ast::ExprKind::Ident(ast::Ident {
            name: inner.to_string(),
            span,
        }))
    };
    let missing = expr(ast::ExprKind::Binary {
        op: ast::BinaryOp::StrictEq,
        lhs: Box::new(read()),
        rhs: Box::new(expr(ast::ExprKind::Lit(ast::Lit::Undefined))),
    });
    let value = expr(ast::ExprKind::Cond {
        cond: Box::new(missing),
        then: Box::new(d.clone()),
        els: Box::new(read()),
    });
    ast::Stmt {
        kind: ast::StmtKind::Var(ast::VarDecl {
            kind: ast::VarKind::Let,
            pattern: ast::Pattern {
                id: ast::NodeId(u32::MAX),
                kind: ast::PatternKind::Ident(name.clone()),
                span: name.span,
            },
            ty: Some(ty.clone()),
            init: Some(value),
            span,
        }),
        span,
    }
}

/// Reports each use of an adapted implementation parameter other than calling it, in the
/// implementation and the closures it captures the parameter in (module docs).
pub(crate) fn check_identity(cx: &mut Ctx, adapted: &FxHashSet<Adapted>) {
    let mut reported = FxHashSet::default();
    for a in adapted {
        let found = cx.defs.iter().position(|d| {
            matches!(d, Some(Def::Fn(f)) if (f.span == a.imp.0 || f.span == a.imp.1)
                && f.params.len() > a.param)
        });
        let Some(i) = found else { continue };
        let Some(Def::Fn(f)) = &cx.defs[i] else {
            continue;
        };
        let start = (i, f.params[a.param].local);
        let mut work = vec![start];
        let mut seen = FxHashSet::default();
        let mut bad = vec![];
        while let Some((d, l)) = work.pop() {
            if !seen.insert((d, l)) {
                continue;
            }
            let Some(Def::Fn(f)) = &cx.defs[d] else {
                continue;
            };
            let mut v = Uses {
                tracked: l,
                called: FxHashSet::default(),
                bad: vec![],
                closures: vec![],
            };
            let mut block = f.body.block.clone();
            visit::block(&mut block, &mut v);
            bad.extend(v.bad);
            for c in v.closures {
                if let Some(Def::Fn(cf)) = &cx.defs[c.0 as usize] {
                    for cap in cf.captures.iter().filter(|cap| cap.outer == l) {
                        work.push((c.0 as usize, cap.inner));
                    }
                }
            }
        }
        for at in bad {
            if !reported.insert(at) {
                continue;
            }
            cx.error(
                Diagnostic::error(
                    format!(
                        "the implementation of `{}` uses its parameter `{}` other than by calling it",
                        a.name, a.param_name
                    ),
                    at,
                )
                .with_note(format!(
                    "a signature passes it a function adapted to the implementation's parameter type, a new one for each call: comparing it (`===`, a `Map` key, `includes`), keeping it or passing it on would not see the function the caller passed, as Node would; call `{}` only, or declare the signature's callback with the implementation's parameter types",
                    a.param_name
                )),
            );
        }
    }
}

/// The uses of local `tracked` in one function body.
struct Uses {
    tracked: LocalId,
    /// Spans of the reads that are a call's callee.
    called: FxHashSet<(u32, u32)>,
    bad: Vec<Span>,
    closures: Vec<hir::DefId>,
}

impl VisitMut for Uses {
    fn expr(&mut self, e: &mut hir::Expr) {
        match &e.kind {
            H::Call {
                callee: Callee::Indirect(f),
                ..
            } => {
                if let Some(at) = read_of(f, self.tracked) {
                    self.called.insert((at.lo, at.hi));
                }
            }
            H::Local(l, _) if *l == self.tracked => {
                if !self.called.contains(&(e.span.lo, e.span.hi)) {
                    self.bad.push(e.span);
                }
            }
            H::Closure(d) => self.closures.push(*d),
            _ => {}
        }
    }
}

/// The span of the read of `l` that callee `f` is (through a share), if it is one.
fn read_of(f: &hir::Expr, l: LocalId) -> Option<Span> {
    match &f.kind {
        H::Local(x, _) if *x == l => Some(f.span),
        H::Call {
            callee: Callee::Intrinsic(hir::Intrinsic::Share),
            args,
            ..
        } => args.first().and_then(|a| read_of(a, l)),
        _ => None,
    }
}
