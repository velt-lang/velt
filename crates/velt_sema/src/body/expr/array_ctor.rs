//! JS array constructors: `new Array<T>(n).fill(v)` (n copies of `v`) and
//! `Array.from({ length: n }, (_, i) => f(i))`, rewritten to the prelude's `arrayFilled` /
//! `arrayFromLength` (one allocation each). A bare `new Array<T>(n)` would hold `n` holes
//! (`undefined`), which Velt has no value for, so it asks for `.fill(v)`; likewise the element
//! argument of `Array.from`'s callback is always `undefined` and must go unused.
//! `Array.from(iterable[, (v, i) => ...])` over anything `for...of` takes is a loop pushing
//! each value (`body/consume.rs`).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// `new Array<T>(n).fill(v)` as `arrayFilled<T>(n, v)`, if `object.prop(args)` is that.
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
        // `new Array(...args)` with a tuple: one element is the argument (`args[0]`, the tuple
        // evaluated once); any other count makes an array of those elements, as in JS, which
        // `fill` replaces (`arrayOne(args, arrayFilled(count, v))`). Any other spread stays
        // the call's arguments (an error unless it is a tuple).
        let elem;
        let n = match &n.kind {
            ast::ExprKind::Spread(inner) => {
                let count = self
                    .trial_type(inner)
                    .and_then(|t| match self.cx.ty.kind(t) {
                        hir::TyKind::Tuple(elems) => Some(elems.len()),
                        _ => None,
                    });
                match count {
                    Some(1) => {}
                    Some(k) => {
                        let filled = synthetic_call(
                            velt_common::std_hook!(array_filled),
                            type_args,
                            vec![int_lit(k as u128, span), args[0].clone()],
                            span,
                        );
                        let call_args = vec![(**inner).clone(), filled];
                        let call = synthetic_call(
                            velt_common::std_hook!(array_one),
                            vec![],
                            call_args,
                            span,
                        );
                        return Some(self.expr(&call, exp, Want::Move));
                    }
                    None => {
                        let call_args = vec![n.clone(), args[0].clone()];
                        let call = synthetic_call(
                            velt_common::std_hook!(array_filled),
                            type_args,
                            call_args,
                            span,
                        );
                        return Some(self.expr(&call, exp, Want::Move));
                    }
                }
                elem = first_of(inner);
                &elem
            }
            _ => n,
        };
        let call = match self.trial_type(n).map(|t| (t, self.length_kind(t))) {
            // As in JS, an `n` that is no number makes an array of that one element.
            Some((_, LengthKind::Other)) => {
                return Some(self.array_of_one(&type_args, n, &args[0], exp, span));
            }
            Some((_, LengthKind::Enum(def))) => {
                return Some(self.array_filled_enum(&type_args, n, def, &args[0], exp, span));
            }
            Some((t, LengthKind::Unsupported)) => {
                self.cx.error(
                    velt_common::Diagnostic::error(
                        format!(
                            "`new Array(n)` with an `n` of type `{}`: converting such a value to a length is not supported yet",
                            self.cx.display(t)
                        ),
                        n.span,
                    )
                    .with_note("give `n` the type `number` (or one enum) first"),
                );
                self.check_args_loose(std::slice::from_ref(n));
                self.check_args_loose(args);
                return Some(self.error_expr(span));
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
                velt_common::std_hook!(array_filled),
                type_args,
                vec![as_number(n), args[0].clone()],
                span,
            ),
        };
        Some(self.expr(&call, exp, Want::Move))
    }

    /// `new Array<T>(e).fill(v)` with `e` a value of the numeric enum `def`: its number is the
    /// length (`arrayFilled(<the member's number>, v)`), as in JS.
    fn array_filled_enum(
        &mut self,
        type_args: &[ast::TypeExpr],
        e: &ast::Expr,
        def: hir::DefId,
        v: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let elem_exp = match type_args {
            [t] => Some(self.resolve(t)),
            _ => exp.and_then(|t| match self.cx.ty.kind(t) {
                hir::TyKind::Array(elem) => Some(*elem),
                _ => None,
            }),
        };
        let eh = self.expr(e, None, Want::Move);
        let f64_ = self.cx.ty.f64;
        let n = self.enum_to_dynamic(eh, def, f64_);
        let vh = self.expr(v, elem_exp, Want::Move);
        let vh = match elem_exp {
            Some(t) => self.coerce(vh, t),
            None => vh,
        };
        let elem = vh.ty;
        let ty = self.cx.ty.intern(hir::TyKind::Array(elem));
        let d = self
            .cx
            .prelude_fn(velt_common::std_hook!(array_filled))
            .unwrap_or_else(|| panic!("ICE: std without the hook `arrayFilled`"));
        let kind = hir::ExprKind::Call {
            callee: hir::Callee::Def(d, vec![elem]),
            args: vec![n, vh],
        };
        self.mk(kind, ty, span)
    }

    /// `new Array<T>(x).fill(v)` with an `x` that is no number: as in JS, `x` is evaluated and
    /// the array holds the one element `v` (`arrayOne(x, [v])`, the literal typed `T[]`).
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
            .prelude_fn(velt_common::std_hook!(array_one))
            .unwrap_or_else(|| panic!("ICE: std without the hook `arrayOne`"));
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

    /// `Array.from({ length: n }, (_, i) => e)` as `arrayFromLength(n, (i) => e)`.
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
        let call = synthetic_call(
            velt_common::std_hook!(array_from_length),
            vec![],
            vec![n.clone(), f],
            span,
        );
        self.expr(&call, exp, Want::Move)
    }
}

/// What `new Array(n)` makes of an `n` of some type, as JS decides at run time: `n` elements
/// (a number, or a value whose type is only known at run time), one element (any other value),
/// or either (a union of both kinds).
#[derive(Clone, Copy, PartialEq, Eq)]
enum LengthKind {
    Number,
    /// A numeric enum's value: its member's number.
    Enum(hir::DefId),
    Other,
    Mixed,
    /// A length Velt cannot convert to a `number` yet (a type parameter bounded by `number`,
    /// a union holding an enum).
    Unsupported,
}

impl LengthKind {
    fn join(self, other: LengthKind) -> LengthKind {
        use LengthKind::*;
        match (self, other) {
            (a, b) if a == b => a,
            (Unsupported, _) | (_, Unsupported) => Unsupported,
            (Other | Mixed, _) | (_, Other | Mixed) => Mixed,
            // Numbers of different kinds (a number type and an enum, or two enums).
            _ => Unsupported,
        }
    }
}

impl FnCx<'_, '_> {
    fn length_kind(&mut self, t: TyId) -> LengthKind {
        if self.cx.dynamic(t).is_some() {
            return LengthKind::Number;
        }
        match self.cx.ty.kind(t).clone() {
            hir::TyKind::Op(..) | hir::TyKind::Error | hir::TyKind::Never => {
                return LengthKind::Number
            }
            // A type parameter bounded by a number type would be a length, but its values do
            // not convert to `number` yet; one bounded by a type that is surely no number
            // (`T extends string`) makes one element; an unbounded one may be either.
            hir::TyKind::Param(i) => {
                let bound = self.cx.constraint_of(i);
                return match bound.map(|c| self.length_kind(c)) {
                    Some(LengthKind::Number | LengthKind::Enum(_)) => LengthKind::Unsupported,
                    Some(LengthKind::Other) => LengthKind::Other,
                    _ => LengthKind::Mixed,
                };
            }
            hir::TyKind::Option(inner, _) => {
                return self.length_kind(inner).join(LengthKind::Other);
            }
            _ => {}
        }
        // An enum: its members' values are numbers or strings (both in a heterogeneous one).
        if let hir::TyKind::Adt(d, _) = self.cx.ty.kind(t).clone() {
            let values = self.cx.enum_info(d).filter(|e| !e.is_union).map(|e| {
                let strings = e.variants.iter().filter(|v| v.str_value.is_some()).count();
                (strings, e.variants.len() - strings)
            });
            match values {
                Some((0, _)) => return LengthKind::Enum(d),
                Some((_, 0)) => return LengthKind::Other,
                Some(_) => return LengthKind::Mixed,
                None => {}
            }
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

/// The integer literal `value`.
fn int_lit(value: u128, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Lit(ast::Lit::Int {
            value,
            suffix: None,
        }),
        span,
    }
}

/// `e[0]`.
fn first_of(e: &ast::Expr) -> ast::Expr {
    let zero = int_lit(0, e.span);
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Index {
            object: Box::new(e.clone()),
            index: Box::new(zero),
            optional: false,
        },
        span: e.span,
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
