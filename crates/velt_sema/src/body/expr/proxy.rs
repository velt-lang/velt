//! `new Proxy(target, handler)` and `Reflect.*` (issue #823; the prelude's
//! std/prelude/zz_proxy.vlt, hir_encodings.md "Proxy and Reflect").
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
        let k = match key {
            super::record::RecordKey::Name(p) => {
                super::setters::synth(ast::ExprKind::Lit(ast::Lit::Str(p.name.clone())), p.span)
            }
            super::record::RecordKey::Index(e) | super::record::RecordKey::Computed(e) => {
                match self.symbol_key_of(e) {
                    Some(name) => {
                        super::setters::synth(ast::ExprKind::Lit(ast::Lit::Str(name)), e.span)
                    }
                    None => (*e).clone(),
                }
            }
        };
        self.note_dyn_members();
        Some(self.prelude_fn_call("__anyDelete", vec![object.clone(), k], span))
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
        // A symbol constant (`KEY in o`) names the member it keys.
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
