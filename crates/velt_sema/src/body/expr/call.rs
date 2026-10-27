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
        if optional && !self.is_method_member(callee) {
            return self.optional_chain(callee, span, |s, f| s.call_value(f, args, span));
        }
        if let Some(h) = self.checked_key_call(callee, type_args, args, span) {
            return h;
        }
        if let Some(callee) = self.without_namespace(callee) {
            return self.call(&callee, type_args, args, false, exp, span);
        }
        if let Some(h) = self.math_trunc_div(callee, args, span) {
            return h;
        }
        if let Some(h) = self.math_int32_call(callee, args, span) {
            return h;
        }
        if let Some(h) = self.math_minmax_call(callee, args, span) {
            return h;
        }
        if let Some(h) = self.dynamic_is_array_call(callee, args, span) {
            return h;
        }
        if let Some(h) = self.dynamic_number_test(callee, args, span) {
            return h;
        }
        if let Some(h) = self.object_helper_call(callee, type_args, args, span) {
            return h;
        }
        if let Some(h) = self.object_builtin_call(callee, type_args, args, exp, span) {
            return h;
        }
        if let Some(h) = self.call_apply(callee, type_args, args, exp, span) {
            return h;
        }
        if let Some(h) = self.json_stringify_layout(callee, type_args, args, span) {
            return h;
        }
        if let Some(h) = self.bound_method_call(callee, args, exp, span) {
            return h;
        }
        match &callee.kind {
            ast::ExprKind::Paren(inner) if !matches!(inner.kind, ast::ExprKind::Arrow { .. }) => {
                self.call(inner, type_args, args, false, exp, span)
            }
            ast::ExprKind::Super => self.super_ctor_call(args, span),
            ast::ExprKind::Ident(id) if !self.is_local_name(&id.name) => {
                if self.cx.has_overloads {
                    if let Some(h) = self.overloaded_call(id, type_args, args, exp, span) {
                        return h;
                    }
                }
                self.named_call(id, type_args, args, exp, span)
            }
            // `it.return(undefined)`: TypeScript's `return(value)` takes the generator's return
            // value; Velt's `return()` has none, and `undefined` is what leaving it out passes.
            ast::ExprKind::Member {
                object,
                prop,
                optional,
            } if self.iteration_return_undefined(object, prop, args) => {
                self.member_call(object, prop, *optional, type_args, &[], exp, span)
            }
            ast::ExprKind::Member {
                object,
                prop,
                optional,
            } => self.member_call(object, prop, *optional, type_args, args, exp, span),
            // `o[KEY]()` with a symbol key: the method (or function field) the symbol names.
            ast::ExprKind::Index {
                object,
                index,
                optional,
            } if self.symbol_key_of(index).is_some() => {
                let prop = ast::Ident {
                    name: self.symbol_key_of(index).unwrap_or_default(),
                    span: index.span,
                };
                self.member_call(object, &prop, *optional, type_args, args, exp, span)
            }
            _ => {
                let f = match &callee.kind {
                    ast::ExprKind::Paren(inner) => self.immediate_closure(inner),
                    _ => self.receiver_expr(callee),
                };
                self.call_value_with(f, type_args, args, exp, span)
            }
        }
    }

    /// Is this `it.return(undefined)` on a generator or an iterator of the prelude (issue
    /// #1144)?
    fn iteration_return_undefined(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
    ) -> bool {
        let [arg] = args else {
            return false;
        };
        if prop.name != "return" || crate::body::narrow::nothing_word(arg) != Some("undefined") {
            return false;
        }
        let Some(t) = self.peek_ty(object) else {
            return false;
        };
        let d = match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) | TyKind::Dyn(d, _) => *d,
            _ => return false,
        };
        const PROTOCOL: [&str; 7] = [
            "Generator",
            "AsyncGenerator",
            "Iterator",
            "AsyncIterator",
            "IterableIterator",
            "AsyncIterableIterator",
            "IteratorObject",
        ];
        PROTOCOL
            .iter()
            .any(|n| matches!(self.cx.prelude.get(*n), Some(Item::Def(p)) if *p == d))
    }

    fn immediate_closure(&mut self, e: &ast::Expr) -> hir::Expr {
        // The arrow an adapter calls in place (`callback::immediate_wrapper`), typed by it.
        let exp = match self.immediate_hint {
            Some((span, t)) if span == e.span => {
                self.immediate_hint = None;
                Some(t)
            }
            _ => None,
        };
        self.closure(e, exp, false)
    }

    /// `name(args)` where `name` is not a local.
    pub(super) fn named_call(
        &mut self,
        id: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        match self.lookup_item(&id.name, id.span) {
            Some(Item::Def(d)) => match &self.cx.info[d.0 as usize] {
                DefInfo::Fn(_) if self.cx.prelude_fn("Number") == Some(d) => {
                    self.number_call(d, args, span)
                }
                DefInfo::Fn(_) => self.fn_call(d, &id.name, type_args, args, exp, span),
                DefInfo::Adt(_) if self.cx.prelude_adt("String") == Some(d) => {
                    self.string_call(args, span)
                }
                DefInfo::Adt(_) if self.is_class_def(d) => {
                    self.cx.error(
                        Diagnostic::error(format!("`{}` is a class", id.name), id.span)
                            .with_note(format!("create instances with `new {}(...)`", id.name)),
                    );
                    self.check_args_loose(args);
                    self.error_expr(span)
                }
                _ => {
                    let f = self.with_payload_at(id.span, |s| s.ident_expr(id, None, Want::Borrow));
                    self.call_value_with(f, type_args, args, exp, span)
                }
            },
            _ => self.builtin_call(id, type_args, args, exp, span),
        }
    }

    /// The signature of function `d` called at `span` (its result inferred first if needed).
    pub(crate) fn fn_callable(&mut self, d: DefId, what: String, span: Span) -> Callable {
        crate::body::defaults::param_defaults(self.cx, d);
        let ret = crate::body::returns::ret_of(self.cx, d, span);
        let async_call = self.rejects_through_promise(d);
        let f = self.cx.fn_info(d);
        let js_numbers = self.cx.scopes[f.module].is_std && !self.cx.scopes[self.module].is_std;
        let js_api = self.is_js_api(d);
        let positions = js_numbers && super::numbers::is_simd_method(&f.name);
        let rest = crate::body::defaults::has_rest(self.cx, d);
        let mut c = Callable {
            what,
            params: f.params.clone(),
            ret,
            slot_names: f.generics.names.clone(),
            bounds: f.generics.bounds.clone(),
            constraints: f.generics.constraints.clone(),
            js_numbers,
            positions,
            js_api,
            rest,
            defaults: f.generics.defaults.clone(),
        };
        if async_call {
            c.ret = self.async_call_ret(d, c.ret);
        }
        if self.is_generator_fn(d) {
            let span = self.cx.fn_info(d).name_span;
            c.ret = self.generator_call_ret(d, c.ret, span);
        }
        c
    }

    /// Explicit `<T, U>` type args fill the trailing `own` slots. Slots they leave out take
    /// their `defaults` (`f<number>()` of `function f<A, B = A[]>()`), as in TypeScript: not
    /// inferred.
    pub(super) fn explicit_type_args(
        &mut self,
        slots: &mut [Option<TyId>],
        own: usize,
        type_args: &[ast::TypeExpr],
        defaults: &[Option<TyId>],
        span: Span,
    ) {
        if type_args.is_empty() {
            return;
        }
        let start = slots.len() - own;
        let required = (start..slots.len())
            .rposition(|k| defaults.get(k).is_none_or(|d| d.is_none()))
            .map_or(0, |i| i + 1);
        if type_args.len() < required || type_args.len() > own {
            let expected = match required == own {
                true => format!("{own}"),
                false => format!("{required} to {own}"),
            };
            self.cx.err(
                format!(
                    "expected {expected} type argument(s), found {}",
                    type_args.len()
                ),
                span,
            );
            return;
        }
        for (i, t) in type_args.iter().enumerate() {
            slots[start + i] = Some(self.resolve(t));
        }
        let spans = type_args
            .iter()
            .enumerate()
            .map(|(i, t)| (start + i, t.span));
        self.explicit_type_spans = Some((span, spans.collect()));
        for k in start + type_args.len()..slots.len() {
            let Some(Some(d)) = defaults.get(k) else {
                break;
            };
            let Some(before) = slots[..k].iter().copied().collect::<Option<Vec<TyId>>>() else {
                break;
            };
            slots[k] = Some(self.cx.subst(*d, &before));
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
        if self.takes_this(d) {
            return self.this_fn_call(d, name, None, type_args, args, exp, span);
        }
        self.check_gated_call(d, name, span);
        let c = self.fn_callable(d, format!("function `{name}`"), span);
        let n = c.slot_names.len();
        let mut slots = vec![None; n];
        self.explicit_type_args(&mut slots, n, type_args, &c.defaults, span);
        self.std_callback_arg(d, args);
        let ck = self.check_call(&c, slots, args, exp, span);
        self.void_task = None;
        self.task_callback = None;
        self.thread_task = None;
        self.thread_callback = None;
        if name == "structuredClone" && Some(d) == self.cx.prelude_fn(name) {
            if let Some(a) = ck.args.first() {
                self.check_structured_clone(a.ty, a.span);
            }
        }
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
        let f = match self.dynamic_apply(f, args, span) {
            Ok(h) => return h,
            Err(f) => f,
        };
        let f = self.fn_bound_value(f).unwrap_or_else(|f| f);
        // A generic function value: a call of its interface's one method (`crate::generic_fns`).
        if let Some(g) = self.cx.generic_sig(f.ty) {
            let s = super::iface_call::IfaceSlot {
                on_param: false,
                iface: g.iface,
                iface_args: g.args,
                slot: 0,
                method: g.method,
                rest: g.rest,
            };
            return self.iface_call(f, s, &[], args, None, span);
        }
        // A function with properties (#921): a call of the function it holds.
        if self.cx.is_callable_object(f.ty) {
            return match self.call_of_callable(f, span) {
                Some(f) => self.call_value(f, args, span),
                None => self.error_expr(span),
            };
        }
        let (params, ret, throws, rest) = match self.cx.ty.kind(f.ty).clone() {
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => (params, ret, throws, rest),
            TyKind::Error | TyKind::Never => {
                self.check_args_loose(args);
                return self.error_expr(span);
            }
            _ => {
                let f = match self.call_fn_union(f, args, span) {
                    Ok(h) => return h,
                    Err(f) => f,
                };
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
            // A module-level `const` holding the closure, called from a function.
            H::Global(g) => match self.cx.global(*g) {
                Some(g) if !g.mutable => match g.init.as_ref().map(|i| &i.kind) {
                    Some(H::Closure(c)) => Some(*c),
                    _ => None,
                },
                _ => None,
            },
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
            let mut default = closure
                .and_then(|d| self.cx.fn_info(d).params.get(i))
                .and_then(|p| p.default.clone());
            // A rest parameter without arguments is the empty array; an optional one
            // (`(a?: number) => R`) is `undefined`.
            if rest && i + 1 == params.len() {
                default = Some(self.mk(H::ArrayLit(vec![]), *ty, span));
            } else if default.is_none()
                && matches!(self.cx.ty.kind(*ty), TyKind::Option(_, n) if n.has_undefined())
            {
                default = Some(self.mk(H::Lit(hir::Lit::Null), *ty, span));
            } else if default.is_none() && i >= args.len() && self.cx.dynamic(*ty).is_some() {
                // A left-out argument of a dynamic type is `undefined`, as for a function whose
                // value the context typed without its `?` (`const g = d` for `d(x?: any)`).
                let undefined = hir::Nothing::Undefined;
                default = Some(self.dynamic_nothing(undefined, *ty, span));
            }
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
            constraints: vec![],
            js_numbers: false,
            positions: false,
            js_api: false,
            rest,
            defaults: vec![],
        };
        let ck = self.check_call(&c, vec![], args, None, span);
        if throws != self.cx.ty.never {
            self.throw_src(crate::defs::ThrowSrc::Direct(throws, span));
        }
        // Through a function type written without `throws` in a user module, it may throw what
        // is stored into the type (`throws::fn_types`).
        if !self.cx.scopes[self.module].is_std {
            let site = self.site_of_value(&f);
            self.throw_src(crate::defs::ThrowSrc::Indirect(f.ty, site, span));
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
    pub(super) fn member_call(
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
        if self.reject_argv_mutation_call(object, prop) {
            self.check_args_loose(args);
            return self.error_expr(span);
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
                s.exempt_cast_receiver(object, &recv);
                let call = s.method_call_on(recv, prop, type_args, args, None, span);
                s.clear_cast_receiver();
                call
            });
        }
        // `t.m(…)` on a narrowed `unknown`: the member read from the value held, called.
        if let Some(f) = self.narrowed_dynamic_member(object, prop, Want::Borrow, span) {
            return self.call_value(f, args, span);
        }
        let recv = match &object.kind {
            ast::ExprKind::Super => return self.super_method_call(prop, args, exp, span),
            _ => {
                // A narrowed union that may hold a view stays the union (`union_view`).
                self.view_receiver = super::member::is_name(object);
                let recv = self.receiver_expr(object);
                self.view_receiver = false;
                recv
            }
        };
        if self.record_internal_call(&recv, prop) {
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let recv = match self.optional_fn_member_call(object, recv, prop, args, span) {
            Ok(call) => return call,
            Err(recv) => recv,
        };
        let recv = self.nullable_helper_receiver(recv, &prop.name);
        let r = self.in_place_receiver(object, recv);
        self.exempt_cast_receiver(object, &r.recv);
        let call = self.method_call_on(r.recv, prop, type_args, args, exp, span);
        self.clear_cast_receiver();
        self.after_receiver(r.before, call)
    }
}
