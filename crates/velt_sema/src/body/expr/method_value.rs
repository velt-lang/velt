//! A method read as a value (`xs.forEach(obj.log)`, `const f = obj.log`) and
//! `obj.method.bind(obj)` (#746): a function value bound to the receiver,
//!
//! ```text
//! { const <bound receiver> = obj;
//!   (p0, …, pn) => <bound receiver>.method(p0, …, pn) }
//! ```
//!
//! The receiver is evaluated once, where the method is read, like `bind` in JS (`this` is
//! captured as it is). JS loses `this` on an unbound read (`obj.log` called later has `this`
//! undefined, so a method that uses it throws a TypeError); Velt binds it to the object it was
//! read from in both forms, so the two only differ where JS would throw.
//!
//! The parameter types come from the function type expected there when it is known (so the
//! value converts to it like an arrow would), else from the method's signature. Where a
//! function returning nothing is expected, the method's result is dropped.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::method::Resolved;
use super::setters::synth;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, TyId, TyKind, UseMode};

const BOUND: &str = "<bound receiver>";

/// What a method value calls: its parameter types and result.
struct Sig {
    params: Vec<TyId>,
    /// Per parameter, its default when it is a literal (`"!"`, `null` for `x?: T`), which the
    /// function value takes too; the value takes no parameters from one with another default.
    defaults: Vec<Option<ast::Expr>>,
    ret: TyId,
}

impl FnCx<'_, '_> {
    /// Is `obj.name` a method (not a field or getter) of `obj`'s type?
    pub(super) fn is_method_value(&mut self, t: TyId, name: &str) -> bool {
        if self.record_args(t).is_some() || self.field_of(t, name).is_some() {
            return false;
        }
        match self.resolve_method(t, name) {
            Some(Resolved::Builtin(_)) | None => false,
            Some(r) => !self.is_getter(&r),
        }
    }

    /// `object.method.bind(this_arg)`: the method value bound to `this_arg`, when `this_arg`
    /// is `object` (the same variable, `this` or path of fields). `None`: not this shape.
    pub(super) fn bound_method_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Member {
            object: inner,
            prop: bind,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &strip_parens(inner).kind
        else {
            return None;
        };
        if bind.name != "bind" {
            return None;
        }
        let t = self.peek_ty(object)?;
        if !self.is_method_value(t, &prop.name) {
            return None;
        }
        let fix = |s: &mut Self| s.arrow_fix(object, object, prop);
        match args {
            [this_arg] if same_path(object, this_arg) => {}
            [this_arg] => {
                let other = self.arrow_fix(object, this_arg, prop);
                let d = Diagnostic::error(
                    format!("`.bind` of method `{}` to another object", prop.name),
                    span,
                )
                .with_note("a method is bound to the object it is read from: `o.m.bind(o)`")
                .with_note(format!("call it on the other object instead: `{other}`"));
                self.cx.error(d);
                return Some(self.error_expr(span));
            }
            _ => {
                let d = Diagnostic::error(
                    "`.bind` of a method takes exactly the object it is read from",
                    span,
                )
                .with_note(format!(
                    "for other arguments, use an arrow function: `{}`",
                    fix(self)
                ));
                self.cx.error(d);
                return Some(self.error_expr(span));
            }
        }
        let obj = self.expr(object, None, Want::Borrow);
        Some(self.method_value(obj, object, prop, exp, span))
    }

    /// The type `e` (a variable, `this` or a path of fields) has, without checking it.
    fn peek_ty(&mut self, e: &ast::Expr) -> Option<TyId> {
        if !is_path(e) {
            return None;
        }
        // Checked again where it is used: its errors are reported there.
        let mark = self.cx.diags.len();
        let h = self.expr(e, None, Want::Borrow);
        self.cx.diags.truncate(mark);
        (!self.cx.ty.is_bottom(h.ty)).then_some(h.ty)
    }

    /// The method `prop` of `obj` (checked from `object`) as a function value bound to it.
    pub(super) fn method_value(
        &mut self,
        mut obj: hir::Expr,
        object: &ast::Expr,
        prop: &ast::Ident,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let Some(sig) = self.method_sig(obj.ty, prop, span) else {
            let d = Diagnostic::error(
                format!("method `{}` cannot be used as a function value", prop.name),
                prop.span,
            )
            .with_note(
                "a generic method, or one with a rest parameter, has no single function type",
            )
            .with_note(format!(
                "wrap it in an arrow function: `{}`",
                self.arrow_fix(object, object, prop)
            ));
            self.cx.error(d);
            return self.error_expr(span);
        };
        let expected = exp.and_then(|e| match self.cx.ty.kind(e) {
            TyKind::FnPtr { params, ret, .. } => Some((params.clone(), *ret)),
            _ => None,
        });
        let unit = self.cx.ty.unit;
        // Parameters: those of the expected function type (the value is called with these),
        // up to the method's; else the method's own, with their literal defaults.
        let (params, n, defaults, ret) = match expected {
            Some((ps, r)) if !ps.iter().any(|p| self.cx.ty.has_error(*p)) => {
                let n = ps.len().min(sig.params.len());
                let ret = if r == unit { unit } else { sig.ret };
                (ps, n, vec![], ret)
            }
            other => {
                let n = sig.defaults.len();
                let ret = match other {
                    Some((_, r)) if r == unit => unit,
                    _ => sig.ret,
                };
                (sig.params[..n].to_vec(), n, sig.defaults.clone(), ret)
            }
        };
        let discard = ret == unit && sig.ret != unit;
        let error = self.cx.ty.error;
        let fn_ty = self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws: error,
        });
        let this = matches!(strip_parens(object).kind, ast::ExprKind::This);
        let mut stmts = vec![];
        self.push_scope();
        let recv = if this {
            synth(ast::ExprKind::This, object.span)
        } else {
            set_place_mode(&mut obj, UseMode::Move);
            let (ty, at) = (obj.ty, obj.span);
            let l = self.new_local(BOUND, ty, false, at, LocalKind::Temp);
            let scope = self.f.scopes.last_mut().expect("ICE: no scope");
            scope.names.insert(BOUND.to_string(), l);
            stmts.push(hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: l,
                    init: Some(obj),
                },
                span: at,
            });
            synth(
                ast::ExprKind::Ident(ast::Ident {
                    name: BOUND.into(),
                    span: at,
                }),
                at,
            )
        };
        let arrow = bound_arrow(recv, prop, n, &defaults, discard, span);
        let value = self.expr(&arrow, Some(fn_ty), Want::Move);
        self.pop_scope();
        let ty = value.ty;
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    /// The parameter types and result of method `prop` on values of type `t`, when it has one
    /// function type (not generic, no rest parameter).
    fn method_sig(&mut self, t: TyId, prop: &ast::Ident, span: Span) -> Option<Sig> {
        match self.resolve_method(t, &prop.name)? {
            Resolved::Def { def, slots, .. } | Resolved::Virtual { def, slots, .. } => {
                let slots: Option<Vec<TyId>> = slots.into_iter().collect();
                let slots = slots?;
                let c = self.fn_callable(def, format!("`{}`", prop.name), span);
                if c.rest || c.slot_names.len() > slots.len() {
                    return None;
                }
                let params = c.params.iter().map(|p| self.cx.subst(p.ty, &slots));
                let params = params.collect();
                let ret = self.cx.subst(c.ret, &slots);
                let defaults = match self.cx.fn_info(def).source {
                    Some(src) => literal_defaults(&crate::body::defaults::fn_sig_ast(src).params),
                    None => c.params.iter().map(|_| None).collect(),
                };
                Some(Sig {
                    params,
                    defaults,
                    ret,
                })
            }
            Resolved::Iface {
                iface_args, method, ..
            } => {
                if !method.generics.names.is_empty() {
                    return None;
                }
                let params = method
                    .params
                    .iter()
                    .map(|p| self.cx.subst(p.ty, &iface_args));
                let params: Vec<TyId> = params.collect();
                let ret = self.cx.subst(method.ret, &iface_args);
                let generic = |s: &Self, t: TyId| s.cx.mentions_params(t);
                if params.iter().any(|p| generic(self, *p)) || generic(self, ret) {
                    return None;
                }
                let defaults = params.iter().map(|_| None).collect();
                Some(Sig {
                    params,
                    defaults,
                    ret,
                })
            }
            Resolved::Builtin(_) => None,
        }
    }

    /// `(x) => recv.prop(x)` with the parameter names of `object`'s method, for notes.
    fn arrow_fix(&mut self, object: &ast::Expr, recv: &ast::Expr, prop: &ast::Ident) -> String {
        let recv = path_text(recv).unwrap_or_else(|| "obj".into());
        let names = self.param_names(object, prop).join(", ");
        format!("({names}) => {recv}.{}({names})", prop.name)
    }

    /// The method's parameter names (`...xs` for a rest parameter).
    fn param_names(&mut self, object: &ast::Expr, prop: &ast::Ident) -> Vec<String> {
        let Some(t) = self.peek_ty(object) else {
            return vec!["x".into()];
        };
        match self.resolve_method(t, &prop.name) {
            Some(Resolved::Def { def, .. } | Resolved::Virtual { def, .. }) => {
                let f = self.cx.fn_info(def);
                match f.source {
                    Some(src) => {
                        let ps = &crate::body::defaults::fn_sig_ast(src).params;
                        let dots = |rest: bool| if rest { "..." } else { "" };
                        let name = |p: &ast::Param| format!("{}{}", dots(p.rest), p.name.name);
                        ps.iter().map(name).collect()
                    }
                    None => f.params.iter().map(|p| p.name.clone()).collect(),
                }
            }
            Some(Resolved::Iface { method, .. }) => {
                method.params.iter().map(|p| p.name.clone()).collect()
            }
            _ => vec!["x".into()],
        }
    }
}

/// Per parameter, its default when that is a literal; the list ends before the first
/// parameter with another default (which the function value does not take).
fn literal_defaults(params: &[ast::Param]) -> Vec<Option<ast::Expr>> {
    let mut out = vec![];
    for p in params {
        match &p.default {
            None => out.push(None),
            Some(e) if is_literal(e) => out.push(Some(e.clone())),
            Some(_) => break,
        }
    }
    out
}

fn is_literal(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(_) => true,
        ast::ExprKind::Paren(x) => is_literal(x),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        } => matches!(expr.kind, ast::ExprKind::Lit(_)),
        _ => false,
    }
}

/// `(#m0, …) => recv.prop(#m0, …)`, its parameters taking `defaults`; with `discard`, a block
/// body that drops the result.
fn bound_arrow(
    recv: ast::Expr,
    prop: &ast::Ident,
    n: usize,
    defaults: &[Option<ast::Expr>],
    discard: bool,
    span: Span,
) -> ast::Expr {
    let name = |k: usize| ast::Ident {
        name: format!("#m{k}"),
        span,
    };
    let params = (0..n)
        .map(|k| ast::ArrowParam {
            name: name(k),
            ty: None,
            default: defaults.get(k).cloned().flatten(),
            optional: false,
        })
        .collect();
    let callee = synth(
        ast::ExprKind::Member {
            object: Box::new(recv),
            prop: prop.clone(),
            optional: false,
        },
        span,
    );
    let call = synth(
        ast::ExprKind::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args: (0..n)
                .map(|k| synth(ast::ExprKind::Ident(name(k)), span))
                .collect(),
            optional: false,
        },
        span,
    );
    let body = if discard {
        ast::ArrowBody::Block(ast::Block {
            stmts: vec![ast::Stmt {
                kind: ast::StmtKind::Expr(call),
                span,
            }],
            span,
        })
    } else {
        ast::ArrowBody::Expr(Box::new(call))
    };
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

fn strip_parens(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) => strip_parens(x),
        _ => e,
    }
}

/// A variable, `this`, or a path of fields of one.
fn is_path(e: &ast::Expr) -> bool {
    match &strip_parens(e).kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_path(object),
        _ => false,
    }
}

/// `a.b.c` for a path (`None` for other expressions).
fn path_text(e: &ast::Expr) -> Option<String> {
    match &strip_parens(e).kind {
        ast::ExprKind::Ident(x) => Some(x.name.clone()),
        ast::ExprKind::This => Some("this".into()),
        ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } => Some(format!("{}.{}", path_text(object)?, prop.name)),
        _ => None,
    }
}

/// Do `a` and `b` name the same variable, `this` or path of fields?
fn same_path(a: &ast::Expr, b: &ast::Expr) -> bool {
    match (&strip_parens(a).kind, &strip_parens(b).kind) {
        (ast::ExprKind::Ident(x), ast::ExprKind::Ident(y)) => x.name == y.name,
        (ast::ExprKind::This, ast::ExprKind::This) => true,
        (
            ast::ExprKind::Member {
                object: x,
                prop: p,
                optional: false,
            },
            ast::ExprKind::Member {
                object: y,
                prop: q,
                optional: false,
            },
        ) => p.name == q.name && same_path(x, y),
        _ => false,
    }
}
