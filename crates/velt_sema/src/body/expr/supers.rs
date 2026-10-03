//! `super(args)` (base constructor, first statement of a derived constructor) and
//! `super.method(args)` (the base implementation, called directly).

use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::defs::ThrowSrc;
use crate::hir::{self, Callee, ExprKind as H, PassMode, TyId, TyKind};
use velt_common::Span;

impl FnCx<'_, '_> {
    /// `super(args)`: the base class constructor on `this`, first statement of a constructor.
    pub(super) fn super_ctor_call(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let base = self.this_base();
        let ok = self.f.super_ok;
        let Some(base) = base.filter(|_| ok) else {
            self.cx.err(
                "`super(...)` must be the first statement of a constructor of a class that `extends` another",
                span,
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        self.f.super_called = true;
        let (bd, bargs) = self.cx.class_of(base).expect("ICE: base class");
        let Some(ctor) = self.cx.adt(bd).and_then(|a| a.ctor) else {
            if !args.is_empty() {
                self.cx.err(
                    "the base class has no constructor; `super()` takes no arguments",
                    span,
                );
            }
            return self.unit_expr(span);
        };
        let owner = self.cx.fn_info(ctor).owner.expect("ICE: ctor owner");
        let ctor_ty = self.ancestor(base, owner);
        let ctor_args = match self.cx.ty.kind(ctor_ty) {
            TyKind::Adt(_, a) => a.clone(),
            _ => bargs,
        };
        let c = self.fn_callable(ctor, "the base class constructor".into());
        let slots = ctor_args.iter().map(|t| Some(*t)).collect();
        let ck = self.check_call(&c, slots, args, None, span);
        let this = self.this_expr(Want::BorrowMut, span);
        let recv = self.receiver(this, Some(ctor_ty), PassMode::BorrowMut);
        let mut all = vec![recv];
        all.extend(ck.args);
        self.throw_src(ThrowSrc::Call(ctor, ck.type_args.clone(), span));
        // The field initializers of this class (and of those between it and `owner`) run
        // right after the base constructor returns (lowering's ctor_init.rs).
        let this_ty = self.this_ty();
        for s in self.class_default_throws(this_ty, Some(owner), span) {
            self.throw_src(s);
        }
        let kind = H::Call {
            callee: Callee::Def(ctor, ck.type_args),
            args: all,
        };
        self.mk(kind, self.cx.ty.unit, span)
    }

    /// `super.method(args)`: the base class's implementation, called directly.
    pub(super) fn super_method_call(
        &mut self,
        prop: &ast::Ident,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let Some(base) = self.this_base() else {
            self.cx.err(
                "`super` is only available in classes that `extends` another",
                span,
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let (bd, bargs) = self.cx.class_of(base).expect("ICE: base");
        let found = crate::collect::lookup_method(self.cx, bd, &bargs, &prop.name);
        let Some(found) = found.filter(|f| !f.is_static()) else {
            self.cx.err(
                format!("no method `{}` in the base class", prop.name),
                prop.span,
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let recv = self.this_expr(Want::Borrow, span);
        let def = found.def();
        self.cx
            .rec_ref(prop.span, crate::ide::record::Target::Def(def));
        let private_to = self.fn_private_to(def);
        self.check_private(private_to, &prop.name, prop.span);
        let owner_args = found.owner_args();
        let slots = self
            .cx
            .fn_info(def)
            .generics
            .names
            .iter()
            .enumerate()
            .map(|(i, _)| owner_args.get(i).copied())
            .collect();
        let recv_ty = found.recv_ty(self.cx);
        self.def_method_call(recv, def, slots, recv_ty, &[], args, exp, span)
    }

    /// Base class type of the enclosing method's `this`.
    pub(super) fn this_base(&mut self) -> Option<TyId> {
        let t = self.this_ty();
        self.cx.base_of(t)
    }

    /// The type of the enclosing method's `this` (the error type outside methods).
    pub(crate) fn this_ty(&mut self) -> TyId {
        match self
            .f
            .scopes
            .first()
            .and_then(|s| s.names.get("this").copied())
        {
            Some(l) => self.local_ty(l),
            None => self.cx.ty.error,
        }
    }
}
