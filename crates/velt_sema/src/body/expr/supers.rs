//! `super(args)` (base constructor, first statement of a derived constructor) and
//! `super.method(args)` (the base implementation, called directly).

use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::defs::{FnKind, ThrowSrc};
use crate::hir::{self, Callee, ExprKind as H, PassMode, TyId, TyKind};
use velt_common::{Diagnostic, Span};

impl FnCx<'_, '_> {
    /// `super(args)`: the base class constructor on `this`, first statement of a constructor.
    pub(super) fn super_ctor_call(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let base = self.this_base();
        let ok = std::mem::take(&mut self.f.super_ok);
        let Some(base) = base.filter(|_| ok) else {
            self.misplaced_super(base.is_some(), span);
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
        let outer = std::mem::replace(&mut self.super_args, true);
        let ck = self.check_call(&c, slots, args, None, span);
        self.super_args = outer;
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

    /// Reports a `super(...)` call where it is not allowed (`derived`: the class extends another).
    fn misplaced_super(&mut self, derived: bool, span: Span) {
        let ctor_frame = self.outer.iter().rposition(|f| f.kind == FnKind::Ctor);
        let d = if self.f.kind == FnKind::Closure && ctor_frame.is_some() {
            // The constructor itself is reported here, not as missing its call.
            if let Some(i) = ctor_frame {
                self.outer[i].super_called = true;
            }
            Diagnostic::error("`super(...)` cannot be called inside a function", span).with_note(
                "the base constructor must run exactly once, before `this` is used: call `super(...)` as the first statement of the constructor itself",
            )
        } else if !derived {
            Diagnostic::error(
                "`super(...)` can only be called in the constructor of a class that `extends` another",
                span,
            )
        } else if self.f.kind != FnKind::Ctor {
            Diagnostic::error("`super(...)` can only be called in a constructor", span)
                .with_note("to call a base class method, write `super.method(...)`")
        } else if self.f.super_first {
            // Reported once: the constructor does call `super(...)`, just not on every path.
            self.f.super_called = true;
            Diagnostic::error(
                "`super(...)` must be a statement of its own at the top of the constructor",
                span,
            )
            .with_note("inside a condition, a loop or another expression, the base constructor could run never or more than once")
            .with_note("call `super(...);` unconditionally as the first statement, and compute its arguments with expressions such as `c ? a : b`")
        } else if self.f.super_called {
            Diagnostic::error("`super(...)` is called more than once", span)
                .with_note("the base constructor must run exactly once; remove this call")
        } else {
            self.f.super_called = true;
            Diagnostic::error("`super(...)` must be the first statement of the constructor", span)
                .with_note("the statements before it would run before the base constructor has initialized the object; move them after the call")
        };
        self.cx.error(d);
    }

    /// `super.method(args)`: the base class's implementation, called directly.
    pub(super) fn super_method_call(
        &mut self,
        prop: &ast::Ident,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let outer = std::mem::replace(&mut self.super_args, false);
        let call = self.super_method_call_inner(prop, args, exp, span);
        if outer {
            self.cx.error(
                Diagnostic::error(
                    format!("`super.{}(...)` cannot be called before `super(...)` has run", prop.name),
                    span,
                )
                .with_note("the base constructor has not initialized the object yet: compute the arguments of `super(...)` without `this` or `super`"),
            );
        }
        self.super_args = outer;
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

    /// `this` (or `super.m()`) inside the arguments of `super(...)`: the object is not
    /// initialized yet.
    pub(crate) fn check_this_ready(&mut self, span: Span) {
        if self.super_args {
            self.cx.error(
                Diagnostic::error(
                    "`this` cannot be used before `super(...)` has run",
                    span,
                )
                .with_note("the base constructor has not initialized the object yet: compute the arguments of `super(...)` without `this`"),
            );
        }
    }

    /// Base class type of the enclosing method's `this`.
    pub(super) fn this_base(&mut self) -> Option<TyId> {
        let l = self.f.scopes.first()?.names.get("this").copied()?;
        let t = self.local_ty(l);
        self.cx.base_of(t)
    }
}
