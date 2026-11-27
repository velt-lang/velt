//! Method calls that pass their object as `this` to a function expression with a `this: any`
//! parameter (`this_fn_expr.rs`), in a program that has one (it loads std/lang/this_fn/index.velt). A call
//! `o.m(x)` on an `any` is a slot call that does so already (velt_vir this_fn.rs); the other
//! member calls whose function is a dynamic value read here call
//! `callOn(<o.m>, o, [x])`, which passes `o` to such a function and calls any other one
//! plainly:
//!
//! - `o[k](x)` (an `any` `o`, or an element of an `any[]`, `arr[0](x)`);
//! - `o.m?.(x)` (`callOptOn`, which evaluates the arguments only when `o.m` is a function);
//! - `o.m(x)` on a typed `o` whose member `m` is a `Function` or an `any`.
//!
//! `o` is evaluated once: a variable, `this` or a field path is named twice, anything else is
//! held in a temporary. The member is read, then the arguments are evaluated, as in JavaScript.

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::any::Dynamic;
use crate::body::FnCx;
use crate::hir;
use crate::hook_calls::{self as hooks, ty, HookTy};

impl FnCx<'_, '_> {
    /// Does the call `callee(..)` go through `callOn` (module docs)? Only in a user module of
    /// a program with function expressions taking `this`.
    pub(super) fn this_fn_call_site(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        optional: bool,
    ) -> bool {
        if !type_args.is_empty()
            || self.cx.scopes[self.module].is_std
            || self
                .cx
                .prelude_adt(velt_common::std_hook!(this_fn))
                .is_none()
            || self.object_assign_callee(callee)
        {
            return false;
        }
        self.this_fn_member(callee, optional).is_some()
    }

    /// The call `callee(args)` at a site `this_fn_call_site` accepted.
    pub(super) fn this_fn_member_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        optional: bool,
        span: Span,
    ) -> hir::Expr {
        let object = match &callee.kind {
            ast::ExprKind::Index { object, .. } | ast::ExprKind::Member { object, .. } => object,
            _ => unreachable!("ICE: a this-function call of another callee"),
        };
        self.call_on(callee, object, args, optional, span)
    }

    /// The object of `callee` when its call goes through `callOn`.
    fn this_fn_member<'e>(
        &mut self,
        callee: &'e ast::Expr,
        optional: bool,
    ) -> Option<&'e ast::Expr> {
        let (object, index) = match &callee.kind {
            ast::ExprKind::Index {
                object,
                optional: false,
                ..
            } => (object, true),
            ast::ExprKind::Member {
                object,
                optional: false,
                ..
            } => (object, false),
            _ => return None,
        };
        // A declared function's `f.call(...)` / `f.apply(...)` (`call_apply.rs`): not read as
        // a value (which for one with a `this` parameter makes a `ThisFn` calling it, `this_param`).
        if let ast::ExprKind::Ident(id) = &object.kind {
            if self.is_fn_item(id) {
                return None;
            }
        }
        let ot = self.trial_type(object)?;
        let dynamic_object = self.cx.dynamic(ot).is_some();
        let ct = match (&callee.kind, dynamic_object) {
            // `o.m?.(x)` on an `any`: its member is an `any`; `o.m(x)` is the slot call.
            (ast::ExprKind::Member { .. }, true) => return optional.then_some(&**object),
            // A field of a typed object, or a record's value (read without checking `o.m`,
            // which for a method makes a function value calling it).
            (ast::ExprKind::Member { prop, .. }, false) => match self.cx.record_type_args(ot) {
                // A symbol-keyed member of a record is one of its methods (`[RECORD_HAS](k)`,
                // which a `for...in` writes), not one of its values, whose keys are strings.
                Some(_)
                    if prop
                        .name
                        .starts_with(crate::property_order::SYMBOL_KEY_PREFIX) =>
                {
                    return None;
                }
                Some((_, v)) => v,
                None => {
                    // Only an object type or class has fields (a union, an enum: none).
                    let crate::hir::TyKind::Adt(d, _) = *self.cx.ty.kind(ot) else {
                        return None;
                    };
                    self.cx.adt(d)?;
                    let fields = self.cx.public_fields(ot);
                    fields.into_iter().find(|f| f.name == prop.name)?.ty
                }
            },
            _ => self.trial_type(callee)?,
        };
        let callable = matches!(self.cx.dynamic(ct), Some(Dynamic::Any | Dynamic::Function));
        (callable && (index || !dynamic_object)).then_some(&**object)
    }

    /// `callOn(<callee with o>, o, [args])` when the member read is a function, else the plain
    /// call of the value, whose `TypeError` names the callee as Node does (`o[k] is not a
    /// function`) and goes to the program's own `catch` (velt_vir `any_not_fn.rs`); `callOn`'s
    /// own call of a value that is no function would stop the program naming its parameter. An
    /// optional call (`o.m?.(x)`) is `undefined` when the member is `null` or `undefined`, without
    /// evaluating the arguments.
    fn call_on(
        &mut self,
        callee: &ast::Expr,
        object: &ast::Expr,
        args: &[ast::Expr],
        optional: bool,
        span: Span,
    ) -> hir::Expr {
        let build = |lw: &mut Self, o: &ast::Expr| {
            let read = match &callee.kind {
                ast::ExprKind::Index { index, more, .. } => ast::ExprKind::Index {
                    object: Box::new(o.clone()),
                    index: index.clone(),
                    more: more.clone(),
                    optional: false,
                },
                ast::ExprKind::Member { prop, .. } => ast::ExprKind::Member {
                    object: Box::new(o.clone()),
                    prop: prop.clone(),
                    optional: false,
                },
                _ => unreachable!("ICE: a this-function call of another callee"),
            };
            let read = synth(read, callee.span);
            // The member read is held in a temporary of its own: an argument of the call must
            // not reach into another (`arr[0]` and `arr`).
            let f = lw.expr(&read, None, crate::body::Want::Borrow);
            let text = lw.callee_text(&f);
            lw.with_temp("<method>", f, callee.span, span, |lw, f| {
                if let Some(l) = lw.f.scopes.last().and_then(|s| s.names.get("<method>")) {
                    let l = *l;
                    lw.f.chain_texts.push((l, text));
                }
                let call = |lw: &mut Self| lw.call_on_function(f, o, args, span);
                if !optional {
                    return call(lw);
                }
                let Some(any) = lw.any_ty() else {
                    return lw.error_expr(span);
                };
                let null = synth(ast::ExprKind::Lit(ast::Lit::Null), span);
                let nullish = binary(ast::BinaryOp::Eq, f.clone(), null, span);
                let cond = lw.expr(&nullish, Some(lw.cx.ty.bool_), crate::body::Want::Move);
                let undefined = lw.dynamic_nothing(hir::Nothing::Undefined, any, span);
                let els = call(lw);
                let kind = hir::ExprKind::If {
                    cond: Box::new(cond),
                    then: Box::new(undefined),
                    els: Box::new(els),
                };
                lw.mk(kind, any, span)
            })
        };
        // A path of a typed object (`arr` of `arr[0](x)`) is held in a temporary too: passing it
        // as `callOn`'s `any` would move it, and a `try` body that moves an outer variable keeps
        // its `catch` from taking the `TypeError` (catch_any.rs).
        let dynamic = self
            .trial_type(object)
            .is_some_and(|t| matches!(self.cx.dynamic(t), Some(Dynamic::Any)));
        if dynamic && super::method_value::is_path(object) {
            return build(self, object);
        }
        let o = self.expr(object, None, crate::body::Want::Borrow);
        // The temporary of a path is named as the path in a `TypeError` (`arr[0]`).
        let text = super::method_value::is_path(object).then(|| self.callee_text(&o));
        self.with_temp("<this>", o, object.span, span, |lw, o| {
            if let Some(text) = text {
                if let Some(&l) = lw.f.scopes.last().and_then(|s| s.names.get("<this>")) {
                    lw.f.chain_texts.push((l, text));
                }
            }
            build(lw, o)
        })
    }

    /// `typeof f === "function" ? callOn(f, o, <args>) : f(...<args>)`, the arguments evaluated
    /// first, into `<args>` (`f` names the member read, `o` the object).
    fn call_on_function(
        &mut self,
        f: &ast::Expr,
        o: &ast::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let list = synth(ast::ExprKind::Array(args.to_vec()), span);
        let anys = self.cx.ty.array(any);
        let list = self.expr(&list, Some(anys), crate::body::Want::Move);
        let list = self.coerce(list, anys);
        self.with_temp("<args>", list, span, span, |lw, xs| {
            let ty_of = synth(
                ast::ExprKind::Unary {
                    op: ast::UnaryOp::TypeOf,
                    expr: Box::new(f.clone()),
                },
                span,
            );
            let function = synth(ast::ExprKind::Lit(ast::Lit::Str("function".into())), span);
            let is_fn = binary(ast::BinaryOp::StrictEq, ty_of, function, span);
            let cond = lw.expr(&is_fn, Some(lw.cx.ty.bool_), crate::body::Want::Move);
            let (fa, oa) = (ty::Any::of(f.clone()), ty::Any::of(o.clone()));
            let then = hooks::call_on(fa, oa, ty::Array::<ty::Any>::of(xs.clone())).check(lw, span);
            let spread = synth(ast::ExprKind::Spread(Box::new(xs.clone())), span);
            let plain = synth(
                ast::ExprKind::Call {
                    callee: Box::new(f.clone()),
                    type_args: vec![],
                    args: vec![spread],
                    optional: false,
                },
                span,
            );
            let els = lw.expr(&plain, Some(any), crate::body::Want::Move);
            let els = lw.coerce(els, any);
            let then = lw.coerce(then, any);
            let kind = hir::ExprKind::If {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            };
            lw.mk(kind, any, span)
        })
    }
}

/// `lhs <op> rhs`, made up at `span`.
fn binary(op: ast::BinaryOp, lhs: ast::Expr, rhs: ast::Expr, span: Span) -> ast::Expr {
    let kind = ast::ExprKind::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    };
    synth(kind, span)
}
