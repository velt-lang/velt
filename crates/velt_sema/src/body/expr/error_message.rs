//! The reads, writes and defaults of `Error`'s `message` in a program where a class extending
//! `Error` declares `get message()` (`crate::error_message` has the whole story).

use velt_common::{std_hook, Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::error_message::{error_class, has_getter, on, KEY};
use crate::hir::{self, Callee, ExprKind as H, TyKind};
use crate::hook_calls::{self as hooks, ty, HookTy};

impl FnCx<'_, '_> {
    /// A placed default `d` that calls `noMessage()`, as the literal `""` in a program without
    /// the getter.
    pub(crate) fn fold_no_message(&self, d: hir::Expr) -> hir::Expr {
        let H::Call {
            callee: Callee::Def(f, _),
            args,
        } = &d.kind
        else {
            return d;
        };
        if !args.is_empty()
            || on(self.cx)
            || self.cx.prelude_fn(std_hook!(error_no_message)) != Some(*f)
        {
            return d;
        }
        hir::Expr {
            kind: H::Lit(hir::Lit::Str(String::new())),
            ty: d.ty,
            span: d.span,
        }
    }

    /// `obj.message` of an `Error` (`obj` of a class extending it, or an error union) used per
    /// `want`, in a program with the getter: a read is `errorMessage(obj)`; a write through a type
    /// with the getter is reported (`Ok` of an error expression). `Err(obj)` otherwise.
    pub(crate) fn error_message_read(
        &mut self,
        obj: hir::Expr,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !on(self.cx) || self.in_error_message_hook() {
            return Err(obj);
        }
        let Some(error) = error_class(self.cx, obj.ty).or_else(|| {
            let union = self.is_error_union(obj.ty);
            union.then(|| self.cx.prelude_adt("Error")).flatten()
        }) else {
            return Err(obj);
        };
        if want == Want::BorrowMut {
            if has_getter(self.cx, obj.ty) {
                let tn = self.cx.display(obj.ty);
                self.cx.error(
                    Diagnostic::error(
                        format!("cannot assign to `message`: `{tn}` declares only `get message()`"),
                        span,
                    )
                    .with_note("a property with only a getter is read-only (TypeScript's TS2540); an error keeps the message it was made with"),
                );
                return Ok(self.error_expr(span));
            }
            return Err(obj);
        }
        let error_ty = self.cx.ty.intern(TyKind::Adt(error, vec![]));
        let obj = match self.error_union_view(obj) {
            Ok(view) => view,
            Err(obj) if obj.ty == error_ty => obj,
            Err(obj) => self.mk(H::Upcast(Box::new(obj)), error_ty, span),
        };
        let str_ = self.cx.ty.str_;
        let call = hooks::error_message(ty::Error::of(obj)).plain(self.cx, str_, span);
        Ok(call.unwrap_or_else(|| panic!("ICE: `errorMessage` is gone")))
    }

    /// `super.message` in a method of a class extending `Error`, in a program with the getter:
    /// the base's `ERROR_MESSAGE` accessor, called directly (`""` from `Error`, as
    /// `Error.prototype.message`; the base's own getter when it declares one).
    pub(crate) fn super_error_message(
        &mut self,
        prop: &ast::Ident,
        span: Span,
    ) -> Option<hir::Expr> {
        if prop.name != "message" || !on(self.cx) {
            return None;
        }
        let base = self.this_base()?;
        error_class(self.cx, base)?;
        let key = ast::Ident {
            name: KEY.to_string(),
            span: prop.span,
        };
        Some(self.super_method_call(&key, &[], None, span))
    }

    /// Is this the body of `errorMessage` itself, whose read of the field is the own message?
    fn in_error_message_hook(&self) -> bool {
        self.body_def.is_some() && self.body_def == self.cx.prelude_fn(std_hook!(error_message))
    }
}

impl FnCx<'_, '_> {
    /// Is `message` of a `ty` read through `errorMessage` (a class extending `Error` or an error
    /// union, in a program with the getter)? Destructuring reads such a key as a property
    /// (`property_pattern.rs`), never as the field.
    pub(crate) fn message_is_property(&mut self, ty: hir::TyId) -> bool {
        on(self.cx) && (error_class(self.cx, ty).is_some() || self.is_error_union(ty))
    }
}
