//! Calls: dispatch on the callee's shape (builtin, named function, static method, enum variant,
//! `super`, method on a value, function value) and building the HIR call.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::Callable;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::defs::{DefInfo, ParamSig};
use crate::hir::{self, Callee, DefId, ExprKind as H, PassMode, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    #[allow(clippy::too_many_arguments)] // mirrors `ExprKind::Call` plus the checking context
    pub(crate) fn call(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        optional: bool,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        if optional {
            return self.optional_chain(callee, span, |s, f| s.call_value(f, args, span));
        }
        if let Some(callee) = self.without_namespace(callee) {
            return self.call(&callee, type_args, args, false, exp, span);
        }
        if let Some(h) = self.math_trunc_div(callee, args, span) {
            return h;
        }
        if let Some(h) = self.object_helper_call(callee, type_args, args, span) {
            return h;
        }
        match &callee.kind {
            ast::ExprKind::Paren(inner) if !matches!(inner.kind, ast::ExprKind::Arrow { .. }) => {
                self.call(inner, type_args, args, false, exp, span)
            }
            ast::ExprKind::Super => self.super_ctor_call(args, span),
            ast::ExprKind::Ident(id) if !self.is_local_name(&id.name) => {
                self.named_call(id, type_args, args, exp, span)
            }
            ast::ExprKind::Member {
                object,
                prop,
                optional,
            } => self.member_call(object, prop, *optional, type_args, args, exp, span),
            _ => {
                let f = match &callee.kind {
                    ast::ExprKind::Paren(inner) => self.immediate_closure(inner),
                    _ => self.expr(callee, None, Want::Borrow),
                };
                self.call_value(f, args, span)
            }
        }
    }

    fn immediate_closure(&mut self, e: &ast::Expr) -> hir::Expr {
        self.closure(e, None, false)
    }

    /// `name(args)` where `name` is not a local.
    fn named_call(
        &mut self,
        id: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        match self.lookup_item(&id.name, id.span) {
            Some(Item::Def(d)) => match &self.cx.info[d.0 as usize] {
                DefInfo::Fn(_) => self.fn_call(d, &id.name, type_args, args, exp, span),
                DefInfo::Adt(_) if self.is_class_def(d) => {
                    self.cx.error(
                        Diagnostic::error(format!("`{}` is a class", id.name), id.span)
                            .with_note(format!("create instances with `new {}(...)`", id.name)),
                    );
                    self.check_args_loose(args);
                    self.error_expr(span)
                }
                _ => {
                    let f = self.ident_expr(id, None, Want::Borrow);
                    self.call_value(f, args, span)
                }
            },
            _ => self.builtin_call(id, type_args, args, exp, span),
        }
    }

    pub(crate) fn fn_callable(&mut self, d: DefId, what: String) -> Callable {
        crate::body::defaults::param_defaults(self.cx, d);
        let async_call = self.rejects_through_promise(d);
        let f = self.cx.fn_info(d);
        let js_numbers = self.cx.scopes[f.module].is_std && !self.cx.scopes[self.module].is_std;
        let rest = f.source.is_some_and(|s| {
            crate::body::defaults::fn_sig_ast(s)
                .params
                .last()
                .is_some_and(|p| p.rest)
        });
        let mut c = Callable {
            what,
            params: f.params.clone(),
            ret: f.ret,
            slot_names: f.generics.names.clone(),
            bounds: f.generics.bounds.clone(),
            js_numbers,
            rest,
        };
        if async_call {
            c.ret = self.async_call_ret(d, c.ret);
        }
        c
    }

    /// Explicit `<T, U>` type args fill the trailing `own` slots.
    pub(super) fn explicit_type_args(
        &mut self,
        slots: &mut [Option<TyId>],
        own: usize,
        type_args: &[ast::TypeExpr],
        span: Span,
    ) {
        if type_args.is_empty() {
            return;
        }
        if type_args.len() != own {
            self.cx.err(
                format!("expected {own} type argument(s), found {}", type_args.len()),
                span,
            );
            return;
        }
        let start = slots.len() - own;
        for (i, t) in type_args.iter().enumerate() {
            slots[start + i] = Some(self.resolve(t));
        }
    }

    pub(super) fn fn_call(
        &mut self,
        d: DefId,
        name: &str,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let c = self.fn_callable(d, format!("function `{name}`"));
        let n = c.slot_names.len();
        let mut slots = vec![None; n];
        self.explicit_type_args(&mut slots, n, type_args, span);
        let ck = self.check_call(&c, slots, args, exp, span);
        self.note_async_args(d, &ck.args);
        self.call_throws(d, &ck.type_args, ck.ret, span);
        let kind = H::Call {
            callee: Callee::Def(d, ck.type_args),
            args: ck.args,
        };
        self.mk(kind, ck.ret, span)
    }

    /// Call through a function value (`TyKind::FnPtr`).
    pub(crate) fn call_value(&mut self, f: hir::Expr, args: &[ast::Expr], span: Span) -> hir::Expr {
        let (params, ret, throws) = match self.cx.ty.kind(f.ty).clone() {
            TyKind::FnPtr {
                params,
                ret,
                throws,
            } => (params, ret, throws),
            TyKind::Error | TyKind::Never => {
                self.check_args_loose(args);
                return self.error_expr(span);
            }
            _ => {
                let tn = self.cx.display(f.ty);
                self.cx.err(
                    format!("this expression is not callable (it has type `{tn}`)"),
                    f.span,
                );
                self.check_args_loose(args);
                return self.error_expr(span);
            }
        };
        // `const f = (x, y = 1) => …; f(2)`: the closure's defaults fill in left-out arguments.
        let closure = match &f.kind {
            H::Local(l, _) => self.f.closure_consts.get(l).copied(),
            _ => None,
        };
        let mut ps = vec![];
        for (i, ty) in params.iter().enumerate() {
            // Function values: Copy args by value, others by pointer (the callee may modify
            // them; see `ownership::mutation` for how calls through function values are checked).
            let mode = if self.cx.is_copy(*ty) {
                PassMode::Copy
            } else {
                PassMode::Borrow
            };
            let default = closure
                .and_then(|d| self.cx.fn_info(d).params.get(i))
                .and_then(|p| p.default.clone());
            ps.push(ParamSig {
                name: format!("arg{i}"),
                span,
                ty: *ty,
                mode,
                default,
            });
        }
        let c = Callable {
            what: "this function".into(),
            params: ps,
            ret,
            slot_names: vec![],
            bounds: vec![],
            js_numbers: false,
            rest: false,
        };
        let ck = self.check_call(&c, vec![], args, None, span);
        if throws != self.cx.ty.never {
            self.throw_src(crate::defs::ThrowSrc::Direct(throws, span));
        }
        let mut f = f;
        set_place_mode(&mut f, UseMode::Borrow);
        let kind = H::Call {
            callee: Callee::Indirect(Box::new(f)),
            args: ck.args,
        };
        self.mk(kind, ck.ret, span)
    }

    #[allow(clippy::too_many_arguments)] // mirrors `obj.prop<T>(args)` plus the checking context
    fn member_call(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        optional: bool,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        if let Some(h) = self.namespace_call(object, prop, type_args, args, exp, span) {
            return h;
        }
        if let Some(h) = self.process_write_call(object, prop, args, exp, span) {
            return h;
        }
        if let Some(h) = self.array_fill_new(object, prop, args, exp, span) {
            return h;
        }
        if optional {
            return self.optional_chain(object, span, |s, recv| {
                if s.record_internal_call(&recv, prop) {
                    s.check_args_loose(args);
                    return s.error_expr(span);
                }
                s.method_call_on(recv, prop, type_args, args, None, span)
            });
        }
        let recv = match &object.kind {
            ast::ExprKind::Super => return self.super_method_call(prop, args, exp, span),
            _ => self.expr(object, None, Want::Borrow),
        };
        if self.record_internal_call(&recv, prop) {
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        self.method_call_on(recv, prop, type_args, args, exp, span)
    }

    /// Method `name` that an `extend` block of class/struct `d` adds (a static one is called as
    /// `C.make()`), with the number of the block's own type parameters.
    fn extension_static(&self, d: DefId, name: &str) -> Option<(crate::defs::MethodRef, usize)> {
        self.cx.extensions.iter().find_map(|x| {
            let targets_d = matches!(self.cx.ty.kind(x.target), TyKind::Adt(t, _) if *t == d);
            let m = x.methods.get(name).filter(|_| targets_d)?;
            Some((*m, x.generics.len()))
        })
    }

    /// `Type.member(...)`: static methods, enum variants, builtin namespaces.
    fn namespace_call(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Ident(id) = &object.kind else {
            return None;
        };
        if self.is_local_name(&id.name) {
            return None;
        }
        match self.lookup_item(&id.name, id.span) {
            Some(Item::Def(d)) if self.cx.enum_info(d).is_some() => {
                Some(self.variant_value(d, prop, args, exp, span))
            }
            Some(Item::Def(d)) if self.cx.adt(d).is_some() => {
                Some(self.static_call(d, prop, type_args, args, exp, span))
            }
            Some(item) => {
                let d = self.companion_class(&id.name, item)?;
                Some(self.static_call(d, prop, type_args, args, exp, span))
            }
            None if (id.name.as_str(), prop.name.as_str()) == ("Promise", "withResolvers") => {
                Some(self.promise_with_resolvers(type_args, args, exp, span))
            }
            None => self.namespace_builtin(id, prop, args, exp, span),
        }
    }

    fn static_call(
        &mut self,
        d: DefId,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let a = self.cx.adt(d).expect("ICE: adt");
        let cname = a.name.clone();
        let mut owner_generics = a.generics.len();
        let mut m = a.methods.get(&prop.name).copied();
        if m.is_none() {
            if let Some((em, n)) = self.extension_static(d, &prop.name) {
                (m, owner_generics) = (Some(em), n);
            }
        }
        let Some(m) = m.filter(|m| m.is_static) else {
            let what = if m.is_some() {
                "an instance method"
            } else {
                "not a static method"
            };
            self.cx
                .err(format!("`{cname}.{}` is {what}", prop.name), prop.span);
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        self.cx
            .rec_ref(prop.span, crate::ide::record::Target::Def(m.def));
        let private_to = self.fn_private_to(m.def);
        self.check_private(private_to, &prop.name, prop.span);
        let c = self.fn_callable(m.def, format!("`{cname}.{}`", prop.name));
        let n = c.slot_names.len();
        let own = n - owner_generics;
        let mut slots = vec![None; n];
        self.explicit_type_args(&mut slots, own, type_args, span);
        let ck = self.check_call(&c, slots, args, exp, span);
        self.note_async_args(m.def, &ck.args);
        self.call_throws(m.def, &ck.type_args, ck.ret, span);
        let kind = H::Call {
            callee: Callee::Def(m.def, ck.type_args),
            args: ck.args,
        };
        self.mk(kind, ck.ret, span)
    }
}
