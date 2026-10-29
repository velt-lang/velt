//! A class getter read on a class instance brand that may hold a typed proxy (#823; velt_vir
//! proxy_typed.rs), sigx's `signal(new SomeClass())`. A method call on a proxy reads the method
//! through the `get` trap and needs the method itself back, but a getter's read is the
//! property's value: Node runs `get(target, "twice", receiver)` and the answer is what
//! `obj.twice` gives. So in a program that makes proxies, `obj.twice` on such a brand is
//! `IsTypedProxy(obj) ? __brandGetterRead(obj, "twice") as T : obj.<twice getter>()`: a plain
//! instance calls its getter as before, and a proxy answers through its trap, checked as the
//! getter's type. A trap that forwards the getter with `Reflect.get(target, key, receiver)`
//! stops there with a `TypeError` (#1732: Node runs it with `this` the proxy). Typed proxies
//! without a brand keep their getter calls as they were.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, Intrinsic, UseMode};

impl FnCx<'_, '_> {
    /// `obj.name` of the class getter `name` where `obj` may be a typed proxy (module docs);
    /// `Err(obj)` otherwise.
    pub(super) fn proxy_getter_read(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if self.cx.class_of(obj.ty).is_none()
            || !self.program_makes_proxies()
            || !self.may_be_typed_proxy(obj.ty)
        {
            return Err(obj);
        }
        let Some(any) = self.any_ty() else {
            return Err(obj);
        };
        let (ty, recv_span) = (obj.ty, obj.span);
        let mut stmts = vec![];
        let recv = match is_place(&obj) {
            true => obj,
            false => {
                let t = self.new_local("<receiver>", ty, false, recv_span, LocalKind::Temp);
                stmts.push(hir::Stmt {
                    kind: hir::StmtKind::Let {
                        local: t,
                        init: Some(obj),
                    },
                    span: recv_span,
                });
                self.mk(H::Local(t, UseMode::Borrow), ty, recv_span)
            }
        };
        let plain = self.method_call_at(recv.clone(), prop, &[], &[], None, span, true);
        let rty = plain.ty;
        let b = self.cx.ty.bool_;
        let cond = self.intrinsic(Intrinsic::IsTypedProxy, vec![recv.clone()], b, span);
        let shared = self.share_if_place(recv);
        let boxed = match self.box_dynamic(shared, any) {
            Ok(h) | Err(h) => h,
        };
        let str_ = self.cx.ty.str_;
        let key = self.mk(H::Lit(hir::Lit::Str(prop.name.clone())), str_, prop.span);
        self.note_dyn_members();
        let read = self.prelude_hir_call("__brandGetterRead", vec![boxed, key], any, span);
        let through = self.intrinsic(Intrinsic::AnyTo, vec![read], plain.ty, span);
        let pick = self.mk(
            H::If {
                cond: Box::new(cond),
                then: Box::new(through),
                els: Box::new(plain),
            },
            rty,
            span,
        );
        Ok(self.with_temps_value(stmts, pick))
    }
}
