//! Method calls that pass their object as `this` to a function expression with a `this: any`
//! parameter (`this_fn_expr.rs`), in a program that has one (it loads std/lang/this_fn/index.velt). A call
//! `o.m(x)` on an `any` is a slot call that does so already (velt_vir this_fn.rs); the other
//! member calls whose function is a dynamic value read here are one call of the member read,
//! `AnyApplyThis(<o.m>, o, "<text>", x)` (velt_vir this_fn.rs, one glue function per program):
//! it passes `o` to such a function and calls any other value plainly (a proxy through its own
//! apply), and a member that is no function throws the `TypeError` naming the callee as Node
//! does, to the program's `catch` (velt_vir any_not_fn.rs):
//!
//! - `o[k](x)` (an `any` `o`, or an element of an `any[]`, `arr[0](x)`);
//! - `o.m?.(x)` (`undefined` when `o.m` is `null` or `undefined`, without evaluating the
//!   arguments);
//! - `o?.m?.(x)`, `o?.[k]?.(x)`, `o?.[k](x)` on an `any` `o` (`undefined` when `o` is `null` or
//!   `undefined`, without reading the member or evaluating the arguments);
//! - `o.m(x)` on a typed `o` whose member `m` is a `Function` or an `any`.
//!
//! `o` is evaluated once, into a temporary, then the member is read, then the arguments are
//! evaluated, as in JavaScript: an argument that assigns the object's path does not change the
//! `this` of the call.

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::any::Dynamic;
use crate::body::FnCx;
use crate::hir;

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
        let (object, object_optional) = match &callee.kind {
            ast::ExprKind::Index {
                object,
                optional: o,
                ..
            }
            | ast::ExprKind::Member {
                object,
                optional: o,
                ..
            } => (object, *o),
            _ => unreachable!("ICE: a this-function call of another callee"),
        };
        self.call_on(callee, object, args, (object_optional, optional), span)
    }

    /// The object of `callee` when its call goes through `callOn`.
    fn this_fn_member<'e>(
        &mut self,
        callee: &'e ast::Expr,
        optional: bool,
    ) -> Option<&'e ast::Expr> {
        // `o?.m` / `o?.[k]`: an optional read of the object, on an `any` only (below).
        let (object, index, object_optional) = match &callee.kind {
            ast::ExprKind::Index {
                object, optional, ..
            } => (object, true, *optional),
            ast::ExprKind::Member {
                object, optional, ..
            } => (object, false, *optional),
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
        if object_optional && !dynamic_object {
            return None;
        }
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
    /// evaluating the arguments; an optional read of the object (`o?.m?.(x)`, `o?.[k](x)`:
    /// `object_optional`) is `undefined` when the object is, without reading the member either.
    fn call_on(
        &mut self,
        callee: &ast::Expr,
        object: &ast::Expr,
        args: &[ast::Expr],
        (object_optional, optional): (bool, bool),
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
                    lw.f.chain_texts.push((l, text.clone()));
                }
                let call = |lw: &mut Self| lw.call_on_function(f, o, &text, args, span);
                if !optional {
                    return call(lw);
                }
                let Some(any) = lw.any_ty() else {
                    return lw.error_expr(span);
                };
                lw.undefined_if_nullish(f, any, span, call)
            })
        };
        // The object is held in a temporary, read before the member and the arguments: `callOn`
        // reads it after them, and an argument may assign the path (`o[k](o = p)` passes the
        // old `o` as `this`, as JavaScript does). For a path of a typed object (`arr` of
        // `arr[0](x)`), passing it as `callOn`'s `any` would also move it, and a `try` body that
        // moves an outer variable keeps its `catch` from taking the `TypeError` (catch_any.rs).
        let o = self.expr(object, None, crate::body::Want::Borrow);
        // The temporary of a path is named as the path in a `TypeError` (`arr[0]`).
        let text = super::method_value::is_path(object).then(|| self.callee_text(&o));
        self.with_temp("<this>", o, object.span, span, |lw, o| {
            if let Some(text) = text {
                if let Some(&l) = lw.f.scopes.last().and_then(|s| s.names.get("<this>")) {
                    lw.f.chain_texts.push((l, text));
                }
            }
            if !object_optional {
                return build(lw, o);
            }
            let Some(any) = lw.any_ty() else {
                return lw.error_expr(span);
            };
            lw.undefined_if_nullish(o, any, span, |lw| build(lw, o))
        })
    }

    /// `x == null ? undefined : then()` (`x` names a held `any`), an `any`: an optional chain's
    /// short-circuit, which evaluates nothing of `then` when `x` is `null` or `undefined`.
    fn undefined_if_nullish(
        &mut self,
        x: &ast::Expr,
        any: hir::TyId,
        span: Span,
        then: impl FnOnce(&mut Self) -> hir::Expr,
    ) -> hir::Expr {
        let null = synth(ast::ExprKind::Lit(ast::Lit::Null), span);
        let nullish = binary(ast::BinaryOp::Eq, x.clone(), null, span);
        let cond = self.expr(&nullish, Some(self.cx.ty.bool_), crate::body::Want::Move);
        let undefined = self.dynamic_nothing(hir::Nothing::Undefined, any, span);
        let els = then(self);
        let kind = hir::ExprKind::If {
            cond: Box::new(cond),
            then: Box::new(undefined),
            els: Box::new(els),
        };
        self.mk(kind, any, span)
    }

    /// `AnyApplyThis(f, o, text, args)`: one call, which passes `o` as `this` to a function
    /// expression taking it and calls any other value plainly (a proxy through its own apply);
    /// a value that is no function throws Node's `TypeError` naming the callee (`text`: `o[k]
    /// is not a function`) to the program's own `catch`, or stops the program, as any call
    /// through `any` does (velt_vir `any_not_fn.rs`). The arguments are evaluated at the call,
    /// after the object and the member (`f` names the member read, `o` the object, each held).
    fn call_on_function(
        &mut self,
        f: &ast::Expr,
        o: &ast::Expr,
        text: &str,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        self.held_as_any("<fn>", f, any, span, |lw, f| {
            lw.held_as_any("<obj>", o, any, span, |lw, o| {
                let fh = lw.expr(f, Some(any), crate::body::Want::Borrow);
                let oh = lw.expr(o, Some(any), crate::body::Want::Borrow);
                let text = lw.key_lit(text, span);
                let Some(boxed) = lw.dynamic_args(args, any, false) else {
                    return lw.error_expr(span);
                };
                let mut all = vec![fh, oh, text];
                all.extend(boxed);
                lw.note_any_call();
                lw.intrinsic(hir::Intrinsic::AnyApplyThis, all, any, span)
            })
        })
    }

    /// `body` with the held value `x` (a temporary's name) as an `any`: itself when it is one,
    /// else boxed into a temporary of its own (`name`; a typed object `arr`, a `Function` member).
    fn held_as_any(
        &mut self,
        name: &str,
        x: &ast::Expr,
        any: hir::TyId,
        span: Span,
        body: impl FnOnce(&mut Self, &ast::Expr) -> hir::Expr,
    ) -> hir::Expr {
        let t = self.trial_type(x);
        if t.is_some_and(|t| matches!(self.cx.dynamic(t), Some(Dynamic::Any))) {
            return body(self, x);
        }
        let h = self.expr(x, Some(any), crate::body::Want::Move);
        let h = self.coerce(h, any);
        self.with_temp(name, h, span, span, body)
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
