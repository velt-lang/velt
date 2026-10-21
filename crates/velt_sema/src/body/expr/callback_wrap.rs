//! The wrapper arrows of callback adapters (`callback`): `(#arg0, …, ...#args) => f(…)`, whose
//! parameters follow the expected function type ([`Shape`]) and whose call passes them to the
//! wrapped function as its own parameters need ([`Arg`], [`plan`]): one by one, spread into a
//! rest parameter, or read from the expected type's rest arguments (`#args.at(k)`).

use velt_common::Span;
use velt_syntax::ast;

use super::callback::synth;

/// The parameters of a function type: `fixed` ones, then a rest parameter when `rest`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Shape {
    pub fixed: usize,
    pub rest: bool,
}

impl Shape {
    /// The shape of `n` parameters, the last one a rest parameter when `rest`.
    pub(crate) fn of(n: usize, rest: bool) -> Self {
        Shape {
            fixed: n - usize::from(rest && n > 0),
            rest: rest && n > 0,
        }
    }

    /// Every parameter passed on as it is: the fixed ones, then the rest arguments spread.
    pub(crate) fn same(self) -> Plan {
        let mut out: Vec<Arg> = (0..self.fixed).map(Arg::Param).collect();
        if self.rest {
            out.push(Arg::Spread);
        }
        Plan::one(out)
    }
}

/// Where an argument of the wrapped call comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Arg {
    /// The wrapper's fixed parameter `k`.
    Param(usize),
    /// The wrapper's rest arguments, spread.
    Spread,
    /// The wrapper's rest argument `k` (`#args.at(k)`: `undefined` when not passed).
    At(usize),
    /// The wrapper's rest argument `k`, known to be passed (`#args[k]`).
    Index(usize),
    /// The wrapper's rest arguments from `k` on, spread (`...#args.slice(k)`).
    SpreadFrom(usize),
    /// An array of these: the wrapped function's own rest parameter as an ordinary parameter
    /// (`[#arg1, ...#args]`).
    Pack(Vec<Arg>),
}

/// How a wrapper calls the function it wraps: with the arguments of `arms[j]` when it got at
/// least `base + j` rest arguments (the largest such `j`). The arms differ in the wrapped
/// function's parameters with defaults that the rest arguments fill: one left out gets its
/// default, as in JavaScript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub arms: Vec<Vec<Arg>>,
    pub base: usize,
}

impl Plan {
    /// One call with `args`.
    pub(crate) fn one(args: Vec<Arg>) -> Self {
        Plan {
            arms: vec![args],
            base: 0,
        }
    }

    /// How many rest arguments the call reads as `#args.at(k)` (which may be missing).
    pub(crate) fn may_be_missing(&self) -> usize {
        self.arms[0]
            .iter()
            .filter(|a| matches!(a, Arg::At(_)))
            .count()
    }
}

/// How a wrapper of shape `want` calls a function of shape `own` that needs `required` of its
/// fixed parameters and gets at most `passed` of them: each fixed parameter of the wrapper while
/// both have one, then the rest arguments, spread into `own`'s rest parameter or read one by one
/// for `own`'s further fixed parameters (`At` for the required ones, guarded `Index`es for those
/// with defaults).
pub(crate) fn plan(want: Shape, own: Shape, passed: usize, required: usize) -> Plan {
    let head: Vec<Arg> = match own.rest {
        // Every fixed argument: the call packs those past `own`'s fixed ones into its rest array.
        true => (0..want.fixed).map(Arg::Param).collect(),
        false => (0..passed.min(own.fixed).min(want.fixed))
            .map(Arg::Param)
            .collect(),
    };
    if !want.rest {
        return Plan::one(head);
    }
    let last = match own.rest {
        true => own.fixed,
        false => passed.min(own.fixed),
    };
    let unpacked = last.saturating_sub(want.fixed);
    let r = required.saturating_sub(want.fixed).min(unpacked);
    let d = unpacked - r;
    let arms = (0..=d)
        .map(|j| {
            let mut a = head.clone();
            a.extend((0..r).map(Arg::At));
            a.extend((r..r + j).map(Arg::Index));
            if own.rest && j == d {
                a.push(match r + d {
                    0 => Arg::Spread,
                    k => Arg::SpreadFrom(k),
                });
            }
            a
        })
        .collect();
    // With one arm the base says nothing.
    let base = if d == 0 { 0 } else { r };
    Plan { arms, base }
}

/// `#arg{k}` / `#args`: parameter names the wrapped code cannot name.
fn arg_name(k: usize, span: Span) -> ast::Ident {
    ast::Ident {
        name: format!("#arg{k}"),
        span,
    }
}

fn rest_name(span: Span) -> ast::Ident {
    ast::Ident {
        name: "#args".into(),
        span,
    }
}

/// `#args.<method>(k)`.
fn rest_call(method: &str, k: usize, span: Span) -> ast::Expr {
    let callee = synth(
        ast::ExprKind::Member {
            object: Box::new(rest_ident(span)),
            prop: ast::Ident {
                name: method.into(),
                span,
            },
            optional: false,
        },
        span,
    );
    synth(
        ast::ExprKind::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args: vec![int(k, span)],
            optional: false,
        },
        span,
    )
}

/// `#args`.
fn rest_ident(span: Span) -> ast::Expr {
    synth(ast::ExprKind::Ident(rest_name(span)), span)
}

fn int(k: usize, span: Span) -> ast::Expr {
    let lit = ast::Lit::Int {
        value: k as u128,
        suffix: None,
    };
    synth(ast::ExprKind::Lit(lit), span)
}

fn arg_expr(a: &Arg, span: Span) -> ast::Expr {
    let spread = |e: ast::Expr| synth(ast::ExprKind::Spread(Box::new(e)), span);
    match a {
        Arg::Param(k) => synth(ast::ExprKind::Ident(arg_name(*k, span)), span),
        Arg::Spread => spread(rest_ident(span)),
        Arg::At(k) => rest_call("at", *k, span),
        Arg::Index(k) => synth(
            ast::ExprKind::Index {
                object: Box::new(rest_ident(span)),
                index: Box::new(int(*k, span)),
                optional: false,
            },
            span,
        ),
        Arg::SpreadFrom(k) => spread(rest_call("slice", *k, span)),
        Arg::Pack(xs) => synth(
            ast::ExprKind::Array(xs.iter().map(|x| arg_expr(x, span)).collect()),
            span,
        ),
    }
}

/// `(#arg0, …, [...#args]) => callee(args…)` with the parameters of `want`, calling as `plan`
/// says, or with `discard` a block body that keeps the call's value in a hidden `const` (a
/// started promise runs to completion).
pub(crate) fn wrapper(
    callee: ast::Expr,
    want: Shape,
    plan: &Plan,
    discard: bool,
    span: Span,
) -> ast::Expr {
    let call = |args: &[Arg]| {
        synth(
            ast::ExprKind::Call {
                callee: Box::new(callee.clone()),
                type_args: vec![],
                args: args.iter().map(|a| arg_expr(a, span)).collect(),
                optional: false,
            },
            span,
        )
    };
    let mut call_expr = call(&plan.arms[0]);
    for (j, arm) in plan.arms.iter().enumerate().skip(1) {
        let len = synth(
            ast::ExprKind::Member {
                object: Box::new(rest_ident(span)),
                prop: ast::Ident {
                    name: "length".into(),
                    span,
                },
                optional: false,
            },
            span,
        );
        let cond = synth(
            ast::ExprKind::Binary {
                op: ast::BinaryOp::GtEq,
                lhs: Box::new(len),
                rhs: Box::new(int(plan.base + j, span)),
            },
            span,
        );
        call_expr = synth(
            ast::ExprKind::Cond {
                cond: Box::new(cond),
                then: Box::new(call(arm)),
                els: Box::new(call_expr),
            },
            span,
        );
    }
    let call = call_expr;
    let body = if discard {
        let pattern = ast::Pattern {
            id: ast::NodeId(u32::MAX),
            kind: ast::PatternKind::Ident(ast::Ident {
                name: "#started".into(),
                span,
            }),
            span,
        };
        ast::ArrowBody::Block(ast::Block {
            stmts: vec![ast::Stmt {
                kind: ast::StmtKind::Var(ast::VarDecl {
                    kind: ast::VarKind::Const,
                    pattern,
                    ty: None,
                    init: Some(call),
                    span,
                }),
                span,
            }],
            span,
        })
    } else {
        ast::ArrowBody::Expr(Box::new(call))
    };
    let param = |name: ast::Ident, rest: bool| ast::ArrowParam {
        name,
        pattern: None,
        ty: None,
        default: None,
        optional: false,
        rest,
    };
    let mut params: Vec<ast::ArrowParam> = (0..want.fixed)
        .map(|k| param(arg_name(k, span), false))
        .collect();
    if want.rest {
        params.push(param(rest_name(span), true));
    }
    synth(
        ast::ExprKind::Arrow {
            type_params: vec![],
            params,
            ret: None,
            throws: None,
            body,
            is_async: false,
        },
        span,
    )
}

#[cfg(test)]
mod tests {
    use super::{plan, Arg, Plan, Shape};

    #[test]
    fn plans_pass_rest_arguments_as_the_wrapped_function_takes_them() {
        let s = Shape::of;
        let one = |a: Vec<Arg>| Plan::one(a);
        // `(a, b) => sum(a, b)` for `sum(...xs)`: every argument, packed by the call.
        assert_eq!(
            plan(s(2, false), s(1, true), 0, 0),
            one(vec![Arg::Param(0), Arg::Param(1)])
        );
        // `(...xs) => log(...xs)`.
        assert_eq!(plan(s(1, true), s(1, true), 0, 0), one(vec![Arg::Spread]));
        // `(e, ...xs) => f(e, xs.at(0), ...xs.slice(1))` for `f(e, a, ...more)`.
        let p = plan(s(2, true), s(3, true), 2, 2);
        assert_eq!(
            p.arms,
            [vec![Arg::Param(0), Arg::At(0), Arg::SpreadFrom(1)]]
        );
        // `(...xs) => two(xs.at(0), xs.at(1))` for `two(a, b)`.
        assert_eq!(
            plan(s(1, true), s(2, false), 2, 2),
            one(vec![Arg::At(0), Arg::At(1)])
        );
        // `(x, i) => double(x)`.
        assert_eq!(
            plan(s(2, false), s(1, false), 1, 1),
            one(vec![Arg::Param(0)])
        );
        // `(...xs) => max(…)` for `max(a = …, b = …, ...rest)`: by how many were passed.
        let p = plan(s(1, true), s(3, true), 2, 0);
        assert_eq!(p.base, 0);
        assert_eq!(
            p.arms,
            [
                vec![],
                vec![Arg::Index(0)],
                vec![Arg::Index(0), Arg::Index(1), Arg::SpreadFrom(2)],
            ]
        );
    }
}
