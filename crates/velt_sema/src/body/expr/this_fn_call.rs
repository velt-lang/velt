//! Method calls that pass their object as `this` to a function expression with a `this: any`
//! parameter (`this_fn_expr.rs`), in a program that has one (it loads std/this_fn.vlt). A call
//! `o.m(x)` on an `any` is a slot call that does so already (velt_vir this_fn.rs); the other
//! member calls whose function is a dynamic value read here call
//! `__callOn(<o.m>, o, [x])`, which passes `o` to such a function and calls any other one
//! plainly:
//!
//! - `o[k](x)` (an `any` `o`, or an element of an `any[]`, `arr[0](x)`);
//! - `o.m?.(x)` (`__callOptOn`, which evaluates the arguments only when `o.m` is a function);
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

impl FnCx<'_, '_> {
    /// Does the call `callee(..)` go through `__callOn` (module docs)? Only in a user module of
    /// a program with function expressions taking `this`.
    pub(super) fn this_fn_call_site(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        optional: bool,
    ) -> bool {
        if !type_args.is_empty()
            || self.cx.scopes[self.module].is_std
            || self.cx.prelude_adt("__ThisFn").is_none()
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

    /// The object of `callee` when its call goes through `__callOn`.
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
        let ot = self.trial_type(object)?;
        let dynamic_object = self.cx.dynamic(ot).is_some();
        let ct = match (&callee.kind, dynamic_object) {
            // `o.m?.(x)` on an `any`: its member is an `any`; `o.m(x)` is the slot call.
            (ast::ExprKind::Member { .. }, true) => return optional.then_some(&**object),
            // A field of a typed object, or a record's value (read without checking `o.m`,
            // which for a method makes a function value calling it).
            (ast::ExprKind::Member { prop, .. }, false) => match self.cx.record_type_args(ot) {
                Some((_, v)) => v,
                None => {
                    let fields = self.cx.public_fields(ot);
                    fields.into_iter().find(|f| f.name == prop.name)?.ty
                }
            },
            _ => self.trial_type(callee)?,
        };
        let callable = matches!(self.cx.dynamic(ct), Some(Dynamic::Any | Dynamic::Function));
        (callable && (index || !dynamic_object)).then_some(&**object)
    }

    /// `__callOn(<callee with o>, o, [args])` (`__callOptOn` with the arguments in a function
    /// for an optional call).
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
                ast::ExprKind::Index { index, .. } => ast::ExprKind::Index {
                    object: Box::new(o.clone()),
                    index: index.clone(),
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
            let list = synth(ast::ExprKind::Array(args.to_vec()), span);
            // The member read is held in a temporary of its own: an argument of the call must
            // not reach into another (`arr[0]` and `arr`).
            let f = lw.expr(&read, None, crate::body::Want::Borrow);
            lw.with_temp("<method>", f, callee.span, span, |lw, f| match optional {
                false => lw.prelude_fn_call("__callOn", vec![f.clone(), o.clone(), list], span),
                true => {
                    let thunk = synth(
                        ast::ExprKind::Arrow {
                            type_params: vec![],
                            params: vec![],
                            ret: None,
                            throws: None,
                            body: ast::ArrowBody::Expr(Box::new(list)),
                            is_async: false,
                        },
                        span,
                    );
                    lw.prelude_fn_call("__callOptOn", vec![f.clone(), o.clone(), thunk], span)
                }
            })
        };
        if super::method_value::is_path(object) {
            return build(self, object);
        }
        let o = self.expr(object, None, crate::body::Want::Borrow);
        self.with_temp("<this>", o, object.span, span, build)
    }
}
