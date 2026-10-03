//! `super(args)` (base constructor, a statement of a derived constructor) and
//! `super.method(args)` (the base implementation, called directly).

use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::defs::ThrowSrc;
use crate::hir::{self, Callee, ExprKind as H, PassMode, TyId, TyKind};
use velt_common::{Diagnostic, Span};

impl FnCx<'_, '_> {
    /// `super(args)`: the base class constructor on `this`, a statement of its own in a
    /// constructor (`body::ctor`). A base class without a constructor makes it `Lit(Unit)`.
    pub(super) fn super_ctor_call(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let base = self.this_base();
        let ok = self.take_super_ok(span);
        let Some(base) = base.filter(|_| ok) else {
            self.misplaced_super(base.is_some(), span);
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let (bd, bargs) = self.cx.class_of(base).expect("ICE: base class");
        let Some(ctor) = self.cx.adt(bd).and_then(|a| a.ctor) else {
            if !args.is_empty() {
                self.cx.err(
                    "the base class has no constructor; `super()` takes no arguments",
                    span,
                );
            }
            // Lowering initializes the base classes' fields at this statement.
            self.before_super = false;
            return self.unit_expr(span);
        };
        let owner = self.cx.fn_info(ctor).owner.expect("ICE: ctor owner");
        let ctor_ty = self.ancestor(base, owner);
        let ctor_args = match self.cx.ty.kind(ctor_ty) {
            TyKind::Adt(_, a) => a.clone(),
            _ => bargs,
        };
        let c = self.fn_callable(ctor, "the base class constructor".into(), span);
        let slots = ctor_args.iter().map(|t| Some(*t)).collect();
        let ck = self.check_call(&c, slots, args, None, span);
        self.before_super = false;
        let this = self.this_expr(Want::BorrowMut, span);
        let recv = self.receiver(this, Some(ctor_ty), PassMode::BorrowMut);
        let mut all = vec![recv];
        all.extend(ck.args);
        self.throw_src(ThrowSrc::Call(ctor, ck.type_args.clone(), span));
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
        let outer = std::mem::replace(&mut self.before_super, false);
        let call = self.super_method_call_inner(prop, args, exp, span);
        if outer {
            self.cx.error(
                Diagnostic::error(
                    format!("`super.{}(...)` cannot be called before `super(...)` has run", prop.name),
                    span,
                )
                .with_note("the base constructor has not initialized the object yet: call base class methods after the `super(...)` call"),
            );
        }
        self.before_super = outer;
        call
    }

    fn super_method_call_inner(
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
        let l = self.f.scopes.first()?.names.get("this").copied()?;
        let t = self.local_ty(l);
        self.cx.base_of(t)
    }
}
