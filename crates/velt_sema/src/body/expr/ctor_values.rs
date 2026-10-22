//! Classes as values (#943): a class named where a value of a construct signature type
//! (`new (buf: ArrayBuffer) => ArrayBufferView`, `crate::fn_types`) is expected, and `new C(…)`,
//! `v instanceof C` and `C.name` through such a value.
//!
//! The value is the object `{ #new: (#arg0, …) => new C(#arg0, …), #is: (#v) => #v instanceof
//! C, #name: "C" }`, checked as a literal of the construct signature's type: the constructor call
//! follows the signature's parameters (a class whose constructor doesn't take them is an error at
//! the class's name), and its instance converts to the signature's instance type. Its fields
//! are hidden, like a callable object's function (`crate::callables`).
//!
//! - `new C(args)` with a variable (or module constant) `C` of that type calls `C.#new(args)`;
//! - `v instanceof C` calls `C.#is(v)` (`v` converts to `unknown`); it doesn't narrow `v`;
//! - `C.name` reads `C.#name`; `typeof C` is `"function"` (`Ctx::typeof_tag`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::callback::synth;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::fn_types::{IS, NAME, NEW};
use crate::hir::{self, AdtKind, DefId, TyId, TyKind};

fn ident(name: &str, span: Span) -> ast::Ident {
    ast::Ident {
        name: name.into(),
        span,
    }
}

/// The key of a hidden field: no source spells it (an empty span, as `crate::callables` keys).
fn hidden(name: &str, span: Span) -> ast::Ident {
    ident(name, Span::new(span.file, span.lo, span.lo))
}

fn name_expr(name: &str, span: Span) -> ast::Expr {
    synth(ast::ExprKind::Ident(ident(name, span)), span)
}

/// Class `name` as written after `new` or `instanceof`.
fn class_type(name: &str, span: Span) -> ast::TypeExpr {
    ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ident(name, span)],
            args: vec![],
        },
        span,
    }
}

fn arrow(params: Vec<ast::ArrowParam>, body: ast::Expr, span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Arrow {
            type_params: vec![],
            params,
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(body)),
            is_async: false,
        },
        span,
    )
}

fn param(name: ast::Ident, rest: bool) -> ast::ArrowParam {
    ast::ArrowParam {
        name,
        pattern: None,
        ty: None,
        default: None,
        optional: false,
        rest,
    }
}

impl FnCx<'_, '_> {
    /// Class `d`, named by `id`, as a value of type `exp` (module docs); `None` when `d` is not
    /// a class.
    pub(super) fn class_value(
        &mut self,
        d: DefId,
        id: &ast::Ident,
        exp: Option<TyId>,
        want: Want,
    ) -> Option<hir::Expr> {
        let a = self.cx.adt(d).filter(|a| a.kind == AdtKind::Class)?;
        let class = a.name.clone();
        let span = id.span;
        let target = exp
            .map(|t| self.cx.ty.core(t).unwrap_or(t))
            .filter(|t| self.cx.ctor_sig(*t).is_some());
        let Some(target) = target else {
            self.cx.error(
                Diagnostic::error(
                    format!("class `{}` used as a value needs a construct signature type", id.name),
                    span,
                )
                .with_note(format!(
                    "Velt passes a class as a value where a construct signature is expected: `const C: new (x: T) => {class} = {};` (#943)",
                    id.name
                )),
            );
            return Some(self.error_expr(span));
        };
        let f = self.cx.ctor_sig(target)?;
        let TyKind::FnPtr { params, rest, .. } = self.cx.ty.kind(f).clone() else {
            return None;
        };
        let n = params.len();
        let arg = |k: usize| ident(&format!("#arg{k}"), span);
        let ps = (0..n).map(|k| param(arg(k), rest && k + 1 == n)).collect();
        let args = (0..n)
            .map(|k| {
                let e = synth(ast::ExprKind::Ident(arg(k)), span);
                match rest && k + 1 == n {
                    true => synth(ast::ExprKind::Spread(Box::new(e)), span),
                    false => e,
                }
            })
            .collect();
        let new = synth(
            ast::ExprKind::New {
                class: class_type(&id.name, span),
                args,
            },
            span,
        );
        let test = synth(
            ast::ExprKind::InstanceOf {
                expr: Box::new(name_expr("#v", span)),
                ty: class_type(&id.name, span),
            },
            span,
        );
        let name = synth(ast::ExprKind::Lit(ast::Lit::Str(class)), span);
        let lit = synth(
            ast::ExprKind::Object(vec![
                ast::ObjectProp::KeyValue(hidden(NEW, span), arrow(ps, new, span)),
                ast::ObjectProp::KeyValue(
                    hidden(IS, span),
                    arrow(vec![param(ident("#v", span), false)], test, span),
                ),
                ast::ObjectProp::KeyValue(hidden(NAME, span), name),
            ]),
            span,
        );
        Some(self.expr(&lit, Some(target), want))
    }

    /// `X` after `new` or `instanceof` naming a variable or module constant of a construct
    /// signature type: that name as an expression.
    fn ctor_value_named(&mut self, ty: &ast::TypeExpr) -> Option<ast::Expr> {
        let ast::TypeExprKind::Named { path, args } = &ty.kind else {
            return None;
        };
        let ([name], true) = (path.as_slice(), args.is_empty()) else {
            return None;
        };
        let t = match self.peek_local_ty(&name.name) {
            Some(t) => t,
            None => match self.cx.lookup_item_at(self.module, &name.name, name.span)? {
                Item::Def(d) if matches!(self.cx.info[d.0 as usize], DefInfo::Global(_)) => {
                    crate::body::driver::ensure_global(self.cx, d);
                    self.cx.global(d)?.ty
                }
                _ => return None,
            },
        };
        let core = self.cx.ty.core(t).unwrap_or(t);
        self.cx.ctor_sig(core)?;
        Some(name_expr(&name.name, name.span))
    }

    /// `new C(args)` through a class value `C` (module docs).
    pub(super) fn new_through_value(
        &mut self,
        class: &ast::TypeExpr,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let c = self.ctor_value_named(class)?;
        let v = self.expr(&c, None, Want::Borrow);
        Some(self.ctor_field_call(v, NEW, args, span))
    }

    /// `e instanceof C` through a class value `C` (module docs).
    pub(super) fn instanceof_value(
        &mut self,
        e: &ast::Expr,
        ty: &ast::TypeExpr,
        span: Span,
    ) -> Option<hir::Expr> {
        let c = self.ctor_value_named(ty)?;
        // Reading the variable `C` before `e` is evaluated changes nothing JS could observe.
        let v = self.expr(&c, None, Want::Borrow);
        Some(self.ctor_field_call(v, IS, std::slice::from_ref(e), span))
    }

    /// A call of the function in hidden field `field` of class value `v`.
    fn ctor_field_call(
        &mut self,
        v: hir::Expr,
        field: &str,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        if self.cx.ty.is_bottom(v.ty) {
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        match self.field_access(v, &hidden(field, span), Want::Borrow, span) {
            Some(f) => self.call_value(f, args, span),
            None => {
                self.check_args_loose(args);
                self.error_expr(span)
            }
        }
    }

    /// `C.name` of a class value `C` of type `t`: its hidden name field.
    pub(super) fn ctor_member(&mut self, t: TyId, prop: &ast::Ident) -> Option<ast::Ident> {
        (prop.name == "name" && self.cx.ctor_sig(t).is_some()).then(|| hidden(NAME, prop.span))
    }
}
