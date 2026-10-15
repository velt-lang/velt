//! `f.call(thisArg, ...args)` and `f.apply(thisArg, args)` on function values (#1110): the call
//! `f(...args)` (`f(...args)` for `apply`, whose array or tuple argument spreads as in
//! `spread_args.rs`, so a `number[]` fills only a rest parameter, as TypeScript's
//! `strictBindCallApply` requires). `thisArg` is evaluated, after `f` and before the arguments,
//! as JavaScript does.
//!
//! The `this` a call passes matters only to a function that reads it. Velt's functions,
//! arrow functions and static functions of the built-in namespaces read none, so any `thisArg`
//! works for them. A method read as a value (`o.m`) keeps the object it was read from, so its
//! `thisArg` must be that object (`o.m.call(o, x)`), and a function value of unknown origin (a
//! variable, a parameter) is called with `null` or `undefined` only; anything else would call it
//! with another `this` than Node does, which is an error saying it is not supported yet.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `f.call(...)` / `f.apply(...)` where `f` is a function (`None`: another call).
    pub(crate) fn call_apply(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Member {
            object: f,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let apply = match prop.name.as_str() {
            "call" => false,
            "apply" => true,
            _ => return None,
        };
        if !type_args.is_empty() || !self.is_function_target(f) {
            return None;
        }
        let what = format!("`{}`", prop.name);
        let (this_arg, rest) = match args.split_first() {
            Some((t, rest)) => (Some(t), rest),
            None => (None, args),
        };
        let call_args: Vec<ast::Expr> = match (apply, rest) {
            (false, _) => rest.to_vec(),
            (true, []) => vec![],
            (true, [list]) => vec![synth(
                ast::ExprKind::Spread(Box::new(list.clone())),
                list.span,
            )],
            (true, _) => {
                self.cx.err(
                    format!(
                        "`apply` expects 1 or 2 argument(s), but got {} (TypeScript's TS2554)",
                        args.len()
                    ),
                    span,
                );
                self.check_args_loose(args);
                return Some(self.error_expr(span));
            }
        };
        if let Some(t) = this_arg {
            if let Some(why) = self.this_not_ignored(f, t) {
                self.cx.error(
                    Diagnostic::error(
                        format!("{what} with this `this` is not supported yet: {why}"),
                        t.span,
                    )
                    .with_note("call the method on the object it should run on (`o.m(x)`); a function that doesn't use `this` takes `null`"),
                );
                self.check_args_loose(args);
                return Some(self.error_expr(span));
            }
        }
        let mut lets = vec![];
        if let Some(t) = this_arg.filter(|t| !is_nothing(t)) {
            if !is_pure_path(f) {
                self.cx.err(
                    format!("{what} of a function that is computed, with a `this` that is computed too, is not supported yet: store the function in a variable first"),
                    t.span,
                );
                self.check_args_loose(args);
                return Some(self.error_expr(span));
            }
            // Evaluated for its effects, as JavaScript evaluates it; the function ignores it.
            let h = self.expr(t, None, Want::Borrow);
            lets.push(hir::Stmt {
                span: t.span,
                kind: hir::StmtKind::Expr(h),
            });
        }
        let call = synth(
            ast::ExprKind::Call {
                callee: f.clone(),
                type_args: vec![],
                args: call_args,
                optional: false,
            },
            span,
        );
        let h = self.expr(&call, exp, Want::Move);
        Some(self.with_lets(lets, h))
    }

    /// Is `f` a function, so that `f.call` / `f.apply` are `Function.prototype`'s: a function
    /// or arrow literal, a function's name, a static function of a class or namespace, or a
    /// value of a function type?
    fn is_function_target(&mut self, f: &ast::Expr) -> bool {
        let f = strip(f);
        match &f.kind {
            ast::ExprKind::Arrow { .. } | ast::ExprKind::Function(_) => true,
            ast::ExprKind::Ident(id) if self.is_fn_item(id) => true,
            ast::ExprKind::Member {
                object,
                optional: false,
                ..
            } if self.static_namespace(object) => true,
            _ => {
                let t = self.trial_expr(f).ty;
                matches!(
                    self.cx.ty.kind(t),
                    TyKind::Closure(_) | TyKind::FnPtr { .. }
                )
            }
        }
    }

    /// Does `id` name a function declaration (not a variable holding a function)?
    fn is_fn_item(&mut self, id: &ast::Ident) -> bool {
        !self.is_local_name(&id.name)
            && matches!(self.lookup_item(&id.name, id.span), Some(Item::Def(d)) if matches!(self.cx.info[d.0 as usize], DefInfo::Fn(_)))
    }

    /// Is `e` the name of a class or of a built-in namespace (`String`, `Math`), whose members
    /// are static?
    fn static_namespace(&mut self, e: &ast::Expr) -> bool {
        let ast::ExprKind::Ident(id) = &e.kind else {
            return false;
        };
        if self.is_local_name(&id.name) {
            return false;
        }
        match self.lookup_item(&id.name, id.span) {
            Some(Item::Def(d)) => self.cx.adt(d).is_some(),
            Some(_) => false,
            None => matches!(
                id.name.as_str(),
                "String" | "Number" | "Math" | "JSON" | "Array" | "Promise" | "Date" | "console"
            ),
        }
    }

    /// Why calling `f` with `this_arg` as `this` would not be the same as calling it without
    /// (`None`: it is).
    fn this_not_ignored(&mut self, f: &ast::Expr, this_arg: &ast::Expr) -> Option<&'static str> {
        if is_nothing(this_arg) {
            return None;
        }
        let f = strip(f);
        match &f.kind {
            ast::ExprKind::Arrow { .. } | ast::ExprKind::Function(_) => None,
            ast::ExprKind::Ident(id) if self.is_fn_item(id) => None,
            ast::ExprKind::Member { object, .. } if self.static_namespace(object) => {
                // A static method's `this` is its class.
                match same_path(object, this_arg) || self.builtin_namespace(object) {
                    true => None,
                    false => Some("a static method's `this` is its class"),
                }
            }
            ast::ExprKind::Member { object, .. } => match same_path(object, this_arg) {
                true => None,
                false => Some("a method read as a value keeps the object it was read from"),
            },
            _ => Some("a function value may be a method that keeps the object it was read from"),
        }
    }

    /// Is `e` the name of a built-in namespace (no class of the program)?
    fn builtin_namespace(&mut self, e: &ast::Expr) -> bool {
        match &e.kind {
            ast::ExprKind::Ident(id) => self.lookup_item(&id.name, id.span).is_none(),
            _ => false,
        }
    }
}

/// `e` without parentheses, `as` casts and `!`.
fn strip(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(inner)
        | ast::ExprKind::Cast { expr: inner, .. }
        | ast::ExprKind::NonNull(inner) => strip(inner),
        _ => e,
    }
}

/// `null` or `undefined`.
fn is_nothing(e: &ast::Expr) -> bool {
    matches!(
        &strip(e).kind,
        ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined)
    )
}

/// Is `e` a name, `this`, a function literal or a property of a name (no effects to order)?
fn is_pure_path(e: &ast::Expr) -> bool {
    match &strip(e).kind {
        ast::ExprKind::Ident(_)
        | ast::ExprKind::This
        | ast::ExprKind::Arrow { .. }
        | ast::ExprKind::Function(_) => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_pure_path(object),
        _ => false,
    }
}

/// Do `a` and `b` name the same object (`o`, `this`, `o.p.q`)?
fn same_path(a: &ast::Expr, b: &ast::Expr) -> bool {
    match (&strip(a).kind, &strip(b).kind) {
        (ast::ExprKind::Ident(x), ast::ExprKind::Ident(y)) => x.name == y.name,
        (ast::ExprKind::This, ast::ExprKind::This) => true,
        (
            ast::ExprKind::Member {
                object: oa,
                prop: pa,
                optional: false,
            },
            ast::ExprKind::Member {
                object: ob,
                prop: pb,
                optional: false,
            },
        ) => pa.name == pb.name && same_path(oa, ob),
        _ => false,
    }
}
