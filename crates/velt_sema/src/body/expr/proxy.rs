//! `new Proxy(target, handler)` and `Reflect.*` (issue #823; std/proxy.vlt, a global loaded on
//! demand: std/prelude/global/proxy.vlt; hir_encodings.md "Proxy and Reflect").
//!
//! A program that never names `Proxy` or `Reflect` doesn't load them, and no value of it can be a
//! proxy: its operations on `any` call the prelude's helpers (std/prelude/zzz_adapt.vlt), which
//! don't test for one. Once loaded, `proxy_op` picks the versions that do (velt:proxy/ops).
//!
//! A proxy is a dynamic value: the `Proxy` object `new Proxy(...)` builds is converted to `any`
//! at once, so every operation on it goes through the descriptor of the `Proxy` class, whose
//! slots call the handler's traps (velt_vir `lower/any_proxy.rs`). TypeScript gives the
//! expression the target's type `T`; here it is an `any`, which converts to the typed value the
//! context expects: a tagged pointer to the same `Proxy` object (velt_vir
//! `lower/proxy_typed.rs`), so typed reads and writes still run the traps.
//!
//! The prelude's own dynamic operations do not count as the program using `any` members
//! (`FnCx::note_dyn_members`); constructing a `Proxy` or calling `Reflect` in the program's
//! own code does, here.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, DefId, ExprKind as H};

impl FnCx<'_, '_> {
    /// Has the program loaded `Proxy` (a module names it: std/prelude/global/proxy.vlt)? Only
    /// then can a value be a proxy.
    pub(crate) fn uses_proxy(&self) -> bool {
        self.cx.prelude_adt("Proxy").is_some()
    }

    /// The prelude helper `name` for an operation on `any`, or, in a program that has loaded
    /// `Proxy`, its version that asks a proxy's traps (`<name>Proxy`, velt:proxy/ops).
    pub(crate) fn proxy_op(&self, name: &'static str) -> &'static str {
        if !self.uses_proxy() {
            return name;
        }
        match name {
            "__spreadAny" => "__spreadAnyProxy",
            "__anyHasOwn" => "__anyHasOwnProxy",
            "__anyValues" => "__anyValuesProxy",
            "__anyEntries" => "__anyEntriesProxy",
            "__anyDelete" => "__anyDeleteProxy",
            "__HasOwnProperty" => "__HasOwnPropertyProxy",
            _ => panic!("ICE: no proxy version of `{name}`"),
        }
    }

    /// Is `d` the prelude's `Proxy` class, used outside the standard library?
    fn user_proxy(&self, d: DefId) -> bool {
        Some(d) == self.cx.prelude_adt("Proxy") && !self.cx.scopes[self.module].is_std
    }

    /// `new Proxy<T>(...)`: `Proxy` takes no type arguments in Velt (the result is an `any`);
    /// the class name without them.
    pub(crate) fn proxy_class_name(&mut self, class: &ast::TypeExpr) -> Option<ast::TypeExpr> {
        let ast::TypeExprKind::Named { path, args } = &class.kind else {
            return None;
        };
        if path.len() != 1 || path[0].name != "Proxy" || args.is_empty() {
            return None;
        }
        let d = self.cx.prelude_adt("Proxy")?;
        match self.lookup_type_path(path) {
            Some(crate::ctx::Item::Def(x)) if x == d => {}
            _ => return None,
        }
        Some(ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: path.clone(),
                args: vec![],
            },
            span: class.span,
        })
    }

    /// `new Proxy(target, handler)` with a handler typed `any` (`handler as any`): the prelude's
    /// `__proxyNewAny(target, handler)`, which reads the traps the handler has through `any`.
    /// `None` for another `new`.
    pub(crate) fn proxy_new_any(
        &mut self,
        class: &ast::TypeExpr,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::TypeExprKind::Named { path, .. } = &class.kind else {
            return None;
        };
        let [_, handler] = args else {
            return None;
        };
        let d = self.cx.prelude_adt("Proxy")?;
        if path.len() != 1 || path[0].name != "Proxy" || !self.user_proxy(d) {
            return None;
        }
        match self.lookup_type_path(path) {
            Some(crate::ctx::Item::Def(x)) if x == d => {}
            _ => return None,
        }
        let t = self.trial_type(handler)?;
        if self.cx.dynamic(t) != Some(crate::any::Dynamic::Any) {
            return None;
        }
        self.cx.any_impl();
        self.cx.note_dyn_members();
        self.cx.object_copies.proxies = true;
        Some(self.prelude_fn_call("__proxyNewAny", args.to_vec(), span))
    }

    /// The `Proxy` object `h` (`new Proxy(...)` of class `d`) as an `any`; `h` itself for
    /// another class.
    pub(crate) fn proxy_new(&mut self, d: DefId, h: hir::Expr, span: Span) -> hir::Expr {
        if !self.user_proxy(d) {
            return h;
        }
        let Some(defs) = self.cx.any else {
            return h;
        };
        let Some(impl_index) = self.cx.any_impl() else {
            return h;
        };
        // No write is noted here: every write that reaches the target through a proxy starts
        // at a write the program's own code makes (`p.k = v`, `Reflect.set`), noted there.
        self.cx.note_dyn_members();
        self.cx.object_copies.proxies = true;
        let any = self.cx.ty.intern(hir::TyKind::Dyn(defs.any, vec![]));
        let kind = H::ToDyn {
            expr: Box::new(h),
            impl_index,
        };
        self.mk(kind, any, span)
    }

    /// A call of the static method `name` of class `d`: `Reflect.*` in the program's own code
    /// makes it use `any` members (the prelude's bodies read and write through `any`), and
    /// `Reflect.set` writes a field through `any`.
    pub(crate) fn note_reflect_call(&mut self, d: DefId, name: &str, span: Span) {
        if Some(d) == self.cx.prelude_adt("Reflect") && !self.cx.scopes[self.module].is_std {
            self.cx.any_impl();
            self.cx.note_dyn_members();
            if name == "set" {
                self.note_dynamic_write("", span);
            }
        }
    }

    /// Does the program's own code construct a `Proxy` anywhere (`new Proxy(...)`)? Then a
    /// value of a class or object type may be a typed proxy, whose keys its traps list.
    pub(crate) fn program_makes_proxies(&mut self) -> bool {
        if let Some(b) = self.cx.object_copies.makes_proxies {
            return b;
        }
        if !self.uses_proxy() {
            self.cx.object_copies.makes_proxies = Some(false);
            return false;
        }
        struct Find(bool);
        impl<'a> velt_syntax::visit::Visit<'a> for Find {
            fn expr(&mut self, e: &'a ast::Expr) {
                if let ast::ExprKind::New { class, .. } = &e.kind {
                    if let ast::TypeExprKind::Named { path, .. } = &class.kind {
                        self.0 |= path.len() == 1 && path[0].name == "Proxy";
                    }
                }
            }
        }
        let mut f = Find(false);
        for m in self.cx.modules.iter().filter(|m| !m.is_std) {
            velt_syntax::visit::walk_module(&m.ast, &mut f);
        }
        self.cx.object_copies.makes_proxies = Some(f.0);
        f.0
    }

    /// Can a value of type `t` be a typed proxy (a class instance or an object, not a record)
    /// in a program that makes proxies?
    pub(crate) fn may_be_typed_proxy(&mut self, t: hir::TyId) -> bool {
        let object = match self.cx.ty.kind(t) {
            hir::TyKind::Adt(d, _) => {
                let d = *d;
                self.record_args(t).is_none()
                    && self
                        .cx
                        .adt(d)
                        .is_some_and(|a| matches!(a.kind, hir::AdtKind::Class | hir::AdtKind::Anon))
            }
            _ => false,
        };
        object && self.program_makes_proxies()
    }

    /// `Object.keys(x)` / `for (k in x)` of a class instance or an object `x` that may be a
    /// typed proxy: `__proxyKeysOf(x)` when it is one (`Intrinsic::IsTypedProxy`), so a proxy's `ownKeys` and
    /// `getOwnPropertyDescriptor` traps list its keys. `None` otherwise.
    pub(crate) fn proxy_keys(&mut self, call: ast::Expr, arg: &ast::Expr) -> Option<hir::Expr> {
        if self.proxy_keys_inner {
            return None;
        }
        let t = self.trial_type(arg)?;
        if !self.may_be_typed_proxy(t) {
            return None;
        }
        // `IsTypedProxy(x) ? __proxyKeysOf(x) : Object.keys(x)`: one test of a bit, and a plain
        // value's own keys as without proxies.
        let span = call.span;
        let x = self.expr(arg, None, crate::body::Want::Borrow);
        let b = self.cx.ty.bool_;
        let cond = self.intrinsic(hir::Intrinsic::IsTypedProxy, vec![x], b, span);
        self.proxy_keys_inner = true;
        let then = self.prelude_fn_call("__proxyKeysOf", vec![arg.clone()], span);
        let els = self.expr(&call, None, crate::body::Want::Move);
        self.proxy_keys_inner = false;
        let ty = els.ty;
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        };
        Some(self.mk(kind, ty, span))
    }

    /// A call of the prelude function `name` with the checked arguments `args`, of type `ret`.
    pub(crate) fn prelude_hir_call(
        &mut self,
        name: &str,
        args: Vec<hir::Expr>,
        ret: hir::TyId,
        span: Span,
    ) -> hir::Expr {
        let d = self
            .cx
            .prelude_fn(name)
            .unwrap_or_else(|| panic!("ICE: the prelude without `{name}`"));
        let kind = H::Call {
            callee: hir::Callee::Def(d, vec![]),
            args,
        };
        self.mk(kind, ret, span)
    }

    /// `delete obj.k` / `delete obj[k]` where `obj` is an `any` or `object`: the prelude's
    /// `__anyDelete(obj, key)` (a proxy runs its `deleteProperty` trap). `None` when `obj` is not
    /// dynamic.
    pub(super) fn dynamic_delete(
        &mut self,
        object: &ast::Expr,
        key: &super::record::RecordKey<'_>,
        span: Span,
    ) -> Option<hir::Expr> {
        let t = self.trial_type(object)?;
        let dynamic = self.cx.dynamic(t)?;
        if dynamic == crate::any::Dynamic::Unknown {
            self.unknown_use_error(object.span);
            return Some(self.error_expr(span));
        }
        let lit = |name: String, span| {
            super::setters::synth(ast::ExprKind::Lit(ast::Lit::Str(name)), span)
        };
        self.note_dyn_members();
        let k = match key {
            super::record::RecordKey::Name(p) => lit(p.name.clone(), p.span),
            super::record::RecordKey::Index(e) | super::record::RecordKey::Computed(e) => {
                // A symbol constant names the member it keys; a proxy's trap sees the symbol.
                match self.symbol_key_of(e) {
                    Some(name) if self.uses_proxy() => {
                        let args = vec![object.clone(), lit(name, e.span), (*e).clone()];
                        return Some(self.prelude_fn_call("__anyDeleteSym", args, span));
                    }
                    Some(name) => lit(name, e.span),
                    None => (*e).clone(),
                }
            }
        };
        let f = self.proxy_op("__anyDelete");
        Some(self.prelude_fn_call(f, vec![object.clone(), k], span))
    }

    /// `key in obj` where `obj` is an `any`: `Intrinsic::AnyHas` (a proxy runs its `has`
    /// trap). `None` when `obj` is not dynamic. The key is evaluated first, as in JavaScript.
    pub(crate) fn dynamic_in(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        let t = self.trial_expr(rhs).ty;
        let dynamic = self.cx.dynamic(t)?;
        // A symbol constant (`KEY in o`) names the member it keys; a proxy's `has` trap sees the
        // symbol itself.
        if dynamic == crate::any::Dynamic::Any && self.uses_proxy() {
            if let Some(name) = self.symbol_key_of(lhs) {
                self.note_dyn_members();
                let name = super::setters::synth(ast::ExprKind::Lit(ast::Lit::Str(name)), lhs.span);
                let args = vec![rhs.clone(), name, lhs.clone()];
                return Some(self.prelude_fn_call("__anyHasSym", args, span));
            }
        }
        let k = match self.symbol_key_of(lhs) {
            Some(name) => self.str_lit(&name, lhs.span),
            None => self.expr(lhs, None, crate::body::Want::Borrow),
        };
        let mut obj = self.expr(rhs, None, crate::body::Want::Borrow);
        if dynamic == crate::any::Dynamic::Unknown {
            self.unknown_use_error(rhs.span);
            return Some(self.error_expr(span));
        }
        if self.cx.ty.is_bottom(k.ty) || self.cx.ty.is_bottom(obj.ty) {
            return Some(self.error_expr(span));
        }
        crate::body::places::set_place_mode(&mut obj, hir::UseMode::Borrow);
        self.note_dyn_members();
        let str_ = self.cx.ty.str_;
        let kt = self.cx.widened(k.ty);
        let k = if kt == str_ {
            self.coerce(k, str_)
        } else if self.cx.dynamic(k.ty).is_some()
            || self.cx.ty.is_numeric(kt)
            || self.string_or_symbol(k.ty)
        {
            self.coerce(k, obj.ty)
        } else {
            let tn = self.cx.display(k.ty);
            self.cx.error(
                Diagnostic::error(
                    format!("`in` with a `{tn}` key on an `any` is not supported yet (#823)"),
                    lhs.span,
                )
                .with_note("use a `string` or `number` key"),
            );
            return Some(self.error_expr(span));
        };
        let b = self.cx.ty.bool_;
        Some(self.intrinsic(hir::Intrinsic::AnyHas, vec![obj, k], b, span))
    }
}
