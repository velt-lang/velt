//! The methods of an object type's implementation of an interface (`crate::object_ifaces`)
//! filled by a `Function` field: a function expression using `this` (`function (this: any, …)
//! { … }`, `body/expr/this_fn_expr.rs`), called with the object as `this`, as Node calls
//! `o.m(…)`. The method is `__callOn(<the field>, <the object>, [<the arguments>])`
//! (std/this_fn.vlt), its arguments and its result passed through `any`: so each parameter is
//! of a type that goes into an `any` as it is (a number, a string, a boolean, an object, an
//! array, a function, an interface value or a dynamic value; no union, optional or literal type)
//! and the result is `void`, a dynamic value or a number, string or boolean.

use crate::any::Dynamic;
use crate::ctx::Ctx;
use crate::defs::{IfaceMethod, ThrowSrc};
use crate::hir::{
    self, Callee, Def, DefId, ExprKind as H, Intrinsic, LocalId, StmtKind, TyId, TyKind, UseMode,
};

impl Ctx<'_> {
    /// Can a `Function` field fill method `m` (whose interface arguments and implementor are
    /// `full`) through `__callOn` (module docs)?
    pub(crate) fn call_on_fills(&mut self, m: &IfaceMethod, full: &[TyId]) -> bool {
        if self.prelude_fn("__callOn").is_none() || self.any.is_none() {
            return false;
        }
        for p in &m.params {
            let t = self.subst(p.ty, full);
            if !self.boxes_as_is(t) {
                return false;
            }
        }
        let ret = self.subst(m.ret, full);
        ret == self.ty.unit
            || self.dynamic(ret).is_some()
            || matches!(
                self.ty.kind(ret),
                TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str
            )
    }

    /// Does a value of type `t` go into an `any` as it is (module docs)?
    fn boxes_as_is(&self, t: TyId) -> bool {
        if self.lit_value(t).is_some() {
            return false;
        }
        match self.ty.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str => true,
            TyKind::Array(_) | TyKind::Map(..) | TyKind::Dyn(..) | TyKind::FnPtr { .. } => true,
            TyKind::Adt(d, _) => self.adt(*d).is_some(),
            _ => false,
        }
    }

    /// Make synthesized method `def` (of self type `self_ty`, its parameters those of the
    /// interface method) call the `Function` in field `index` (of type `fn_ty`) with the object
    /// as `this` (module docs).
    pub(crate) fn call_on_body(&mut self, def: DefId, self_ty: TyId, index: usize, fn_ty: TyId) {
        let (Some(call_on), Some(any_impl)) = (self.prelude_fn("__callOn"), self.any_impl()) else {
            panic!("ICE: a `this` function slot without std/this_fn");
        };
        let any = self
            .ty
            .intern(TyKind::Dyn(self.any.expect("ICE: any").any, vec![]));
        let list_ty = self.ty.intern(TyKind::Array(any));
        let info = self.fn_info(def);
        let (params, ret, span) = (info.params.clone(), info.ret, info.span);
        let this = |mode| hir::Expr {
            kind: H::Local(LocalId(0), mode),
            ty: self_ty,
            span,
        };
        let share = |e: hir::Expr| hir::Expr {
            ty: e.ty,
            span,
            kind: H::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                args: vec![e],
            },
        };
        let field = hir::Expr {
            kind: H::Field {
                base: Box::new(this(UseMode::Borrow)),
                index: index as u32,
                mode: UseMode::Borrow,
            },
            ty: fn_ty,
            span,
        };
        let f = hir::Expr {
            kind: H::Call {
                callee: Callee::Intrinsic(Intrinsic::AnyTo),
                args: vec![share(field)],
            },
            ty: any,
            span,
        };
        let to_any = |e: hir::Expr| hir::Expr {
            kind: H::ToDyn {
                expr: Box::new(e),
                impl_index: any_impl,
            },
            ty: any,
            span,
        };
        let me = to_any(share(this(UseMode::Borrow)));
        let mut items = vec![];
        for (k, p) in params.iter().enumerate() {
            let copy = self.is_copy(p.ty);
            let read = hir::Expr {
                kind: H::Local(
                    LocalId(k as u32 + 1),
                    if copy { UseMode::Copy } else { UseMode::Borrow },
                ),
                ty: p.ty,
                span,
            };
            let value = if copy { read } else { share(read) };
            items.push(match self.dynamic(p.ty) {
                Some(Dynamic::Any | Dynamic::Unknown) => value,
                _ => to_any(value),
            });
        }
        let list = hir::Expr {
            kind: H::ArrayLit(items),
            ty: list_ty,
            span,
        };
        let call = hir::Expr {
            kind: H::Call {
                callee: Callee::Def(call_on, vec![]),
                args: vec![f, me, list],
            },
            ty: any,
            span,
        };
        let kind = match ret == self.ty.unit || self.dynamic(ret) == Some(Dynamic::Any) {
            true if ret == self.ty.unit => StmtKind::Expr(call),
            true => StmtKind::Return(Some(call)),
            false => StmtKind::Return(Some(hir::Expr {
                kind: H::Call {
                    callee: Callee::Intrinsic(Intrinsic::AnyTo),
                    args: vec![call],
                },
                ty: ret,
                span,
            })),
        };
        self.fn_info_mut(def).throw_srcs = vec![ThrowSrc::Call(call_on, vec![], span)];
        let Some(Def::Fn(f)) = &mut self.defs[def.0 as usize] else {
            panic!("ICE: a synthesized method without a body");
        };
        f.body.block.stmts = vec![hir::Stmt { kind, span }];
    }
}
