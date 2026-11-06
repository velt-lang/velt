//! `new Proxy(target, handler)` and `Reflect.*` (issue #823; std/lang/proxy/index.velt, a global loaded on
//! demand: std/prelude/global/proxy.velt; hir_encodings.md "Proxy and Reflect").
//!
//! A program that never names `Proxy` or `Reflect` doesn't load them, and no value of it can be a
//! proxy: its operations on `any` call the prelude's helpers (std/lang/hooks), which
//! don't test for one. Once loaded, `proxy_op` picks the versions that do (velt/lang/hooks/proxy).
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

pub(crate) use velt_common::std_modules::PROXY_MODULES;

use crate::body::FnCx;
use crate::hir::{self, DefId, ExprKind as H};
use crate::hook_calls::{self as hooks, ty, Call, HookTy};

impl FnCx<'_, '_> {
    /// Has the program loaded `Proxy` (a module names it: std/prelude/global/proxy.velt)? Only
    /// then can a value be a proxy.
    pub(crate) fn uses_proxy(&self) -> bool {
        self.cx.prelude_adt("Proxy").is_some()
    }

    /// The prelude class `name` for an operation on `any`, or, in a program that has loaded
    /// `Proxy`, its version that asks a proxy's traps (`<name>Proxy`, velt/lang/hooks/proxy).
    /// The function hooks with a proxy version are chosen where they are called, by
    /// [`Self::uses_proxy`], through their typed builders (`hook_calls.rs`).
    pub(crate) fn proxy_op(&self, name: &'static str) -> &'static str {
        if !self.uses_proxy() {
            return name;
        }
        match name {
            velt_common::std_hook!(has_own_property) => {
                velt_common::std_hook!(has_own_property_proxy)
            }
            _ => panic!("ICE: no proxy version of `{name}`"),
        }
    }

    /// The `Proxy` method an operation on `any` runs on a proxy, whose errors (its trap's) the
    /// operation throws: `[PROXY_GET]` for a read, `[PROXY_SET]` for a write, `[PROXY_HAS]` for
    /// `in` and `[PROXY_KEYS]` for `Object.keys` (std/lang/hooks/keys.velt). `None` for another
    /// intrinsic.
    pub(crate) fn proxy_method_of(i: hir::Intrinsic) -> Option<&'static str> {
        use hir::Intrinsic as I;
        Some(match i {
            I::AnyGet | I::AnyIndex => velt_common::std_key!(proxy_get),
            I::AnySet | I::AnySetIndex => velt_common::std_key!(proxy_set),
            I::AnyHas => velt_common::std_key!(proxy_has),
            I::AnyKeys => velt_common::std_key!(proxy_keys),
            _ => return None,
        })
    }

    /// Operation `i` on an `any` (`Intrinsic::AnyGet`, ...): in a user module (or std/lang/proxy,
    /// whose `Reflect` functions forward to a target that may be a proxy) of a program that has
    /// loaded `Proxy`, it throws what the `Proxy` method it runs on a proxy throws
    /// (`proxy_method_of`; what the handler's traps throw), as in JavaScript, where an error
    /// thrown by a trap leaves the operation. velt_vir routes that error the same way
    /// (`lower/proxy_throws.rs`). Elsewhere in the standard library, and for the operations
    /// reached through glue, a trap's error ends the process as uncaught (hir_encodings.md
    /// "Proxy and Reflect").
    pub(crate) fn any_op(
        &mut self,
        i: hir::Intrinsic,
        args: Vec<hir::Expr>,
        ty: crate::hir::TyId,
        span: Span,
    ) -> hir::Expr {
        let std = self.cx.scopes[self.module].is_std;
        if !std || PROXY_MODULES.contains(&self.cx.modules[self.module].path.as_str()) {
            let m = Self::proxy_method_of(i).and_then(|name| {
                let p = self.cx.prelude_adt("Proxy")?;
                let a = self.cx.adt(p)?;
                a.methods.get(name).filter(|m| !m.is_static).map(|m| m.def)
            });
            // One source per method in each list: the operations of a body throw the same set.
            let list = self.f.tries.last().unwrap_or(&self.f.uncaught);
            let seen = |s: &crate::defs::ThrowSrc| matches!(s, crate::defs::ThrowSrc::Call(d, a, _) if Some(*d) == m && a.is_empty());
            if let (Some(m), false) = (m, list.iter().any(seen)) {
                self.throw_src(crate::defs::ThrowSrc::Call(m, vec![], span));
            }
        }
        self.intrinsic(i, args, ty, span)
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
    /// `proxyNewAny(target, handler)`, which reads the traps the handler has through `any`.
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
        let target = ty::Any::of(args[0].clone());
        let call = hooks::proxy_new_any(target, ty::Any::of(handler.clone()));
        Some(call.check(self, span))
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
                    && self.cx.adt(d).is_some_and(|a| match a.kind {
                        hir::AdtKind::Class | hir::AdtKind::Anon => true,
                        // A fields-only interface; not a Velt `struct`, a value type.
                        hir::AdtKind::Struct => a.decl.is_none(),
                    })
            }
            _ => false,
        };
        object && self.program_makes_proxies()
    }

    /// `Object.keys(x)` / `for (k in x)` of a class instance or an object `x` that may be a
    /// typed proxy: `proxyKeysOf(x)` when it is one (`Intrinsic::IsTypedProxy`), so a proxy's `ownKeys` and
    /// `getOwnPropertyDescriptor` traps list its keys. `None` otherwise.
    pub(crate) fn proxy_keys(&mut self, call: ast::Expr, arg: &ast::Expr) -> Option<hir::Expr> {
        if self.proxy_keys_inner {
            return None;
        }
        if let ast::ExprKind::Ident(i) = &arg.kind {
            if i.name.starts_with(crate::body::for_in_proxy::FOR_IN_OBJECT) {
                // A `for...in` loop's plain value (for_in_proxy.rs).
                return None;
            }
        }
        let t = self.trial_type(arg)?;
        if !self.may_be_typed_proxy(t) {
            return None;
        }
        // `IsTypedProxy(x) ? proxyKeysOf(x) : Object.keys(x)`: one test of a bit, and a plain
        // value's own keys as without proxies.
        let span = call.span;
        let x = self.expr(arg, None, crate::body::Want::Borrow);
        let b = self.cx.ty.bool_;
        let cond = self.intrinsic(hir::Intrinsic::IsTypedProxy, vec![x], b, span);
        self.proxy_keys_inner = true;
        let then = hooks::proxy_keys_of(ty::Any::of(arg.clone())).check(self, span);
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

    /// `"k" in x` of a class instance or an object `x` that may be a typed proxy:
    /// `IsTypedProxy(x) ? proxyHas(x, "k") : "k" in x`, so a proxy's `has` trap answers and a
    /// plain value keeps its own test (and its type stays uncounted: only a proxy is converted
    /// to `any`). `None` otherwise.
    pub(crate) fn proxy_in(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        if self.proxy_keys_inner || !super::object_keys::reevaluable(rhs) {
            return None;
        }
        let t = self.trial_type(rhs)?;
        if !self.may_be_typed_proxy(t) {
            return None;
        }
        let plain = super::setters::synth(
            ast::ExprKind::Binary {
                op: ast::BinaryOp::In,
                lhs: Box::new(lhs.clone()),
                rhs: Box::new(rhs.clone()),
            },
            span,
        );
        let proxy = hooks::proxy_has(ty::Any::of(rhs.clone()), ty::Str::of(lhs.clone()));
        Some(self.proxy_split(rhs, proxy, &plain, span))
    }

    /// `Object.hasOwn(x, k)` of a class instance or an object `x` that may be a typed proxy:
    /// `IsTypedProxy(x) ? proxyHasOwnOf(x, k) : plain`, so a proxy's
    /// `getOwnPropertyDescriptor` trap answers for `k` alone. `None` otherwise.
    pub(crate) fn proxy_has_own(
        &mut self,
        o: &ast::Expr,
        k: &ast::Expr,
        plain: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        if self.proxy_keys_inner || !super::object_keys::reevaluable(o) {
            return None;
        }
        let t = self.trial_type(o)?;
        if !self.may_be_typed_proxy(t) {
            return None;
        }
        let proxy = hooks::proxy_has_own_of(ty::Any::of(o.clone()), ty::Str::of(k.clone()));
        Some(self.proxy_split(o, proxy, plain, span))
    }

    /// `IsTypedProxy(x) ? proxy : plain`, both checked without the typed proxy hooks.
    fn proxy_split(
        &mut self,
        x: &ast::Expr,
        proxy: Call<ty::Bool, ast::Expr>,
        plain: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let xv = self.expr(x, None, crate::body::Want::Borrow);
        let b = self.cx.ty.bool_;
        let cond = self.intrinsic(hir::Intrinsic::IsTypedProxy, vec![xv], b, span);
        self.proxy_keys_inner = true;
        let then = proxy.check(self, span);
        let els = self.expr(plain, None, crate::body::Want::Move);
        self.proxy_keys_inner = false;
        let ty = els.ty;
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        };
        self.mk(kind, ty, span)
    }

    /// Is `e` a value that may be a typed proxy but is not safe to evaluate again (a call)? The
    /// hooks above then bind it to a temporary first (`with_temp`).
    pub(crate) fn typed_proxy_once(&mut self, e: &ast::Expr) -> bool {
        if self.proxy_keys_inner || super::object_keys::reevaluable(e) {
            return false;
        }
        match self.trial_type(e) {
            Some(t) => self.may_be_typed_proxy(t),
            None => false,
        }
    }

    /// `{ const <operand> = e; f(<operand>) }`: `f` checked with `e` evaluated once.
    pub(crate) fn operand_once(
        &mut self,
        e: &ast::Expr,
        span: Span,
        f: impl FnOnce(&mut Self, &ast::Expr) -> hir::Expr,
    ) -> hir::Expr {
        let h = self.expr(e, None, crate::body::Want::Move);
        self.with_temp("<operand>", h, e.span, span, f)
    }

    /// `h` to spread, a value that may be a typed proxy or a view: a place, checked first
    /// (`proxySpreadCheck`): only the fields of its type are copied.
    pub(super) fn spread_view_check(
        &mut self,
        h: hir::Expr,
        lets: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        let span = h.span;
        let names: Vec<String> = match self.adt_of(h.ty).and_then(|(d, _)| self.cx.adt(d)) {
            Some(a) => a.fields.iter().map(|f| f.name.clone()).collect(),
            None => vec![],
        };
        let h = match crate::body::places::is_place(&h) {
            true => h,
            false => self.temp("<spread>", h, lets),
        };
        if let Some(check) = self.proxy_spread_check(&h, &names) {
            lets.push(hir::Stmt {
                kind: hir::StmtKind::Expr(check),
                span,
            });
        }
        h
    }

    /// `{ ...x }` of the place `x` (of an object type with fields `fields`) that may be a typed
    /// proxy: `if (IsTypedProxy(x)) proxySpreadCheck(x as any, fields)`. `None` with no `any` in
    /// the program.
    fn proxy_spread_check(&mut self, x: &hir::Expr, fields: &[String]) -> Option<hir::Expr> {
        let span = x.span;
        let any = self.any_ty()?;
        let mut x = x.clone();
        crate::body::places::set_place_mode(&mut x, hir::UseMode::Borrow);
        let cond = self.intrinsic(
            hir::Intrinsic::IsTypedProxy,
            vec![x.clone()],
            self.cx.ty.bool_,
            span,
        );
        let x = self.share_if_place(x);
        let boxed = self
            .box_value_dynamic(x, any, crate::any::Dynamic::Any)
            .ok()?;
        let names = ast::ExprKind::Lit(ast::Lit::Str(fields.join("\n")));
        let names = super::setters::synth(names, span);
        let names = self.expr(&names, None, crate::body::Want::Move);
        let unit = self.cx.ty.unit;
        let then = hooks::proxy_spread_check(ty::Any::of(boxed), ty::Str::of(names))
            .emit(self, unit, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(self.unit_expr(span)),
        };
        Some(self.mk(kind, unit, span))
    }

    /// `delete obj.k` / `delete obj[k]` where `obj` is an `any` or `object`: the prelude's
    /// `anyDeleteMember(obj, key)` (a proxy runs its `deleteProperty` trap). `None` when `obj` is not
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
                        let call = hooks::any_delete_sym(
                            ty::Any::of(object.clone()),
                            ty::Str::of(lit(name, e.span)),
                            ty::Symbol::of((*e).clone()),
                        );
                        return Some(call.check(self, span));
                    }
                    Some(name) => lit(name, e.span),
                    None => (*e).clone(),
                }
            }
        };
        let (o, k) = (ty::Any::of(object.clone()), ty::Any::of(k));
        let call = match self.uses_proxy() {
            true => hooks::any_delete_proxy(o, k),
            false => hooks::any_delete_member(o, k),
        };
        Some(call.check(self, span))
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
                let (o, k) = (ty::Any::of(rhs.clone()), ty::Symbol::of(lhs.clone()));
                let call = hooks::any_has_sym(o, ty::Str::of(name), k);
                return Some(call.check(self, span));
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
        Some(self.any_has(obj, k, lhs.span, span))
    }

    /// `key in obj` where `obj`'s type is known only per instantiation (`T`, `T & object`,
    /// issue #1212): `Intrinsic::AnyHas` on the value boxed as an `any`. `None` for any other
    /// `obj`.
    pub(crate) fn generic_in(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        let t = self.trial_expr(rhs).ty;
        if !self.per_instance(t) {
            return None;
        }
        let k = match self.symbol_key_of(lhs) {
            Some(name) => self.str_lit(&name, lhs.span),
            None => self.expr(lhs, None, crate::body::Want::Borrow),
        };
        let obj = self.expr(rhs, None, crate::body::Want::Borrow);
        if self.cx.ty.is_bottom(k.ty) || self.cx.ty.is_bottom(obj.ty) {
            return Some(self.error_expr(span));
        }
        let obj = match self.generic_as_any(obj) {
            Ok(boxed) => boxed,
            Err(_) => return Some(self.error_expr(span)),
        };
        Some(self.any_has(obj, k, lhs.span, span))
    }

    /// `k in obj` of a checked key and `any` (`Intrinsic::AnyHas`).
    fn any_has(&mut self, obj: hir::Expr, k: hir::Expr, key_span: Span, span: Span) -> hir::Expr {
        self.note_dyn_members();
        let str_ = self.cx.ty.str_;
        let kt = self.cx.widened(k.ty);
        let kty_of_key = k.ty;
        let k = if kt == str_ {
            self.coerce(k, str_)
        } else if self.cx.dynamic(k.ty).is_some()
            || self.cx.ty.is_numeric(kt)
            || self.string_or_symbol(k.ty)
            || (self.uses_proxy() && self.symbolic_key_type(k.ty))
        {
            self.coerce(k, obj.ty)
        } else {
            let tn = self.cx.display(k.ty);
            self.cx.error(
                Diagnostic::error(
                    format!("`in` with a `{tn}` key on an `any` is not supported yet (#823)"),
                    key_span,
                )
                .with_note("use a `string` or `number` key"),
            );
            return self.error_expr(span);
        };
        let b = self.cx.ty.bool_;
        if self.uses_proxy() && self.symbolic_key_type(kty_of_key) {
            return hooks::any_has_key(ty::Any::of(obj), ty::Any::of(k)).emit(self, b, span);
        }
        self.any_op(hir::Intrinsic::AnyHas, vec![obj, k], b, span)
    }
}
