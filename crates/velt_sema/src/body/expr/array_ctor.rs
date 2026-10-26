//! JS array constructors: `new Array<T>(n).fill(v)` (n copies of `v`) and
//! `Array.from({ length: n }, (_, i) => f(i))`, rewritten to the prelude's `__arrayFilled` /
//! `__arrayFromLength` (one allocation each). A bare `new Array<T>(n)` would hold `n` holes
//! (`undefined`), which Velt has no value for, so it asks for `.fill(v)`; likewise the element
//! argument of `Array.from`'s callback is always `undefined` and must go unused.
//! `Array.from(iterable[, (v, i) => ...])` over anything `for...of` takes is a loop pushing
//! each value (`body/consume.rs`).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// `new Array<T>(n).fill(v)` as `__arrayFilled<T>(n, v)`, if `object.prop(args)` is that.
    pub(super) fn array_fill_new(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let (type_args, n) = new_array(object)?;
        if prop.name != "fill" || args.len() != 1 {
            return None;
        }
        let call = match self.trial_type(n).map(|t| (t, self.length_kind(t))) {
            // As in JS, an `n` that is no number makes an array of that one element.
            Some((_, LengthKind::Other)) => {
                return Some(self.array_of_one(&type_args, n, &args[0], exp, span));
            }
            Some((t, LengthKind::Mixed)) => {
                self.cx.error(
                    velt_common::Diagnostic::error(
                        format!(
                            "`new Array(n)` with an `n` of type `{}`, which may or may not be a number: JS decides at run time between `n` elements and an array of the one element `n`, and Velt needs to know which",
                            self.cx.display(t)
                        ),
                        n.span,
                    )
                    .with_note(
                        "narrow `n` first: `typeof n === \"number\" ? new Array<T>(n).fill(v) : [v]`",
                    ),
                );
                self.check_args_loose(std::slice::from_ref(n));
                self.check_args_loose(args);
                return Some(self.error_expr(span));
            }
            // A length of any numeric type (or `any`) as a `number`: the prelude checks it the
            // way JS does (a whole number from 0 up, within the array length limit). An `n`
            // with errors reports them here.
            _ => synthetic_call(
                "__arrayFilled",
                type_args,
                vec![as_number(n), args[0].clone()],
                span,
            ),
        };
        Some(self.expr(&call, exp, Want::Move))
    }

    /// `new Array<T>(x).fill(v)` with an `x` that is no number: as in JS, `x` is evaluated and
    /// the array holds the one element `v` (`__arrayOne(x, [v])`, the literal typed `T[]`).
    fn array_of_one(
        &mut self,
        type_args: &[ast::TypeExpr],
        x: &ast::Expr,
        v: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let lit_exp = match type_args {
            [t] => {
                let elem = self.resolve(t);
                Some(self.cx.ty.intern(hir::TyKind::Array(elem)))
            }
            _ => exp,
        };
        let xh = self.expr(x, None, Want::Move);
        let lit = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Array(vec![v.clone()]),
            span,
        };
        let arr = self.expr(&lit, lit_exp, Want::Move);
        let d = self
            .cx
            .prelude_fn("__arrayOne")
            .unwrap_or_else(|| panic!("ICE: the prelude without `__arrayOne`"));
        let ty = arr.ty;
        let kind = hir::ExprKind::Call {
            callee: hir::Callee::Def(d, vec![xh.ty, ty]),
            args: vec![xh, arr],
        };
        self.mk(kind, ty, span)
    }

    /// `new Array<T>(n)` not followed by `.fill(v)`: an error.
    pub(super) fn bare_new_array(&mut self, class: &ast::TypeExpr, span: Span) -> bool {
        let user_class = self.cx.lookup_item_at(self.module, "Array", span).is_some();
        if !is_array_name(class) || user_class {
            return false;
        }
        self.cx.error(
            velt_common::Diagnostic::error(
                "`new Array(n)` would hold `n` empty slots, which Velt has no value for",
                span,
            )
            .with_note(
                "write `new Array<T>(n).fill(value)` or `Array.from({ length: n }, (_, i) => ...)`",
            ),
        );
        true
    }

    /// `Array.from({ length: n }, (_, i) => e)` as `__arrayFromLength(n, (i) => e)`.
    pub(super) fn array_from(
        &mut self,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let (Some(n), Some(f)) = (args.first().and_then(length_of), args.get(1)) else {
            if let (1 | 2, Some(src)) = (args.len(), args.first()) {
                if length_of(src).is_none() {
                    if let Some(h) = self.array_from_iterable(src, args.get(1), span) {
                        return h;
                    }
                    self.cx.error(
                        velt_common::Diagnostic::error(
                            "`Array.from` takes an iterable or `{ length: n }`",
                            src.span,
                        )
                        .with_note("pass an array, a string, a `Map`, a generator or a value with a `[Symbol.iterator]()` method, or write `Array.from({ length: n }, (_, i) => ...)`"),
                    );
                    return self.error_expr(span);
                }
            }
            self.cx.err(
                "`Array.from` supports `Array.from(iterable[, (value, i) => ...])` and `Array.from({ length: n }, (_, i) => ...)`",
                span,
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let Some(f) = index_only(f) else {
            self.cx.err(
                "the first argument of `Array.from`'s callback is always `undefined`: name it `_` (`(_, i) => ...`)",
                f.span,
            );
            return self.error_expr(span);
        };
        let call = synthetic_call("__arrayFromLength", vec![], vec![n.clone(), f], span);
        self.expr(&call, exp, Want::Move)
    }
}

/// What `new Array(n)` makes of an `n` of some type, as JS decides at run time: `n` elements
/// (a number, or a value whose type is only known at run time), one element (any other value),
/// or either (a union of both kinds).
#[derive(Clone, Copy, PartialEq, Eq)]
enum LengthKind {
    Number,
    Other,
    Mixed,
}

impl LengthKind {
    fn join(self, other: LengthKind) -> LengthKind {
        if self == other {
            self
        } else {
            LengthKind::Mixed
        }
    }
}

impl FnCx<'_, '_> {
    fn length_kind(&mut self, t: TyId) -> LengthKind {
        if self.cx.dynamic(t).is_some() {
            return LengthKind::Number;
        }
        match self.cx.ty.kind(t).clone() {
            hir::TyKind::Param(_)
            | hir::TyKind::Op(..)
            | hir::TyKind::Error
            | hir::TyKind::Never => return LengthKind::Number,
            hir::TyKind::Option(inner, _) => {
                return self.length_kind(inner).join(LengthKind::Other);
            }
            _ => {}
        }
        if let Some(members) = self.cx.union_members(t) {
            let mut kinds = members.into_iter().map(|m| self.length_kind(m));
            let first = kinds.next().unwrap_or(LengthKind::Other);
            return kinds.fold(first, LengthKind::join);
        }
        if self.cx.typeof_tag(t) == "number" {
            LengthKind::Number
        } else {
            LengthKind::Other
        }
    }
}

/// `e as number`.
fn as_number(e: &ast::Expr) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Cast {
            expr: Box::new(e.clone()),
            ty: ast::TypeExpr {
                kind: ast::TypeExprKind::Named {
                    path: vec![ast::Ident {
                        name: "number".to_string(),
                        span: e.span,
                    }],
                    args: vec![],
                },
                span: e.span,
            },
        },
        span: e.span,
    }
}

/// `new Array<T>(n)`: (type args, n).
fn new_array(e: &ast::Expr) -> Option<(Vec<ast::TypeExpr>, &ast::Expr)> {
    match &e.kind {
        ast::ExprKind::New { class, args } if is_array_name(class) && args.len() == 1 => {
            match &class.kind {
                ast::TypeExprKind::Named { args: targs, .. } => Some((targs.clone(), &args[0])),
                _ => None,
            }
        }
        ast::ExprKind::Paren(inner) => new_array(inner),
        _ => None,
    }
}

fn is_array_name(t: &ast::TypeExpr) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, .. } if path.len() == 1 && path[0].name == "Array")
}

/// `n` in `{ length: n }`.
fn length_of(e: &ast::Expr) -> Option<&ast::Expr> {
    match &e.kind {
        ast::ExprKind::Object(props) => match props.as_slice() {
            [ast::ObjectProp::KeyValue(k, v)] if k.name == "length" => Some(v),
            _ => None,
        },
        _ => None,
    }
}

/// `(_, i) => e` as `(i) => e` (the element parameter must be named `_…`).
fn index_only(f: &ast::Expr) -> Option<ast::Expr> {
    let ast::ExprKind::Arrow {
        type_params,
        params,
        ret,
        throws,
        body,
        is_async,
    } = &f.kind
    else {
        return None;
    };
    // `() => 0`: a callback that takes neither argument (adapted to the index callback).
    if params.is_empty() {
        return Some(f.clone());
    }
    let [elem, index] = params.as_slice() else {
        return None;
    };
    if !elem.name.name.starts_with('_') {
        return None;
    }
    Some(ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Arrow {
            type_params: type_params.clone(),
            params: vec![index.clone()],
            ret: ret.clone(),
            throws: throws.clone(),
            body: body.clone(),
            is_async: *is_async,
        },
        span: f.span,
    })
}

fn synthetic_call(
    name: &str,
    type_args: Vec<ast::TypeExpr>,
    args: Vec<ast::Expr>,
    span: Span,
) -> ast::Expr {
    let callee = ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Ident(ast::Ident {
            name: name.to_string(),
            span,
        }),
        span,
    };
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Call {
            callee: Box::new(callee),
            type_args,
            args,
            optional: false,
        },
        span,
    }
}
