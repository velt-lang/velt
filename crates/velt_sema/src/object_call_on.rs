//! The methods of an object type's implementation of an interface (`crate::object_ifaces`)
//! filled by a `Function` field: a function expression using `this` (`function (this: any, …)
//! { … }`, `body/expr/this_fn_expr.rs`), called with the object as `this`, as Node calls
//! `o.m(…)`. The method is `callOn(<the field>, <the object>, [<the arguments>])`
//! (std/lang/this_fn/index.vlt), its arguments and its result passed through `any`: so each parameter is
//! of a type that goes into an `any` as it is (a number, a string, a boolean, an object, an
//! array, a function, an interface value or a dynamic value; no union, optional or literal type)
//! and the result is `void`, a dynamic value or a number, string or boolean.
//!
//! Through `any`, an object's field of another type than the interface's (`props:
//! Record<string, any>` for `props: PropsAccessor<{}>`, `renderFn: any` for
//! `renderFn: ViewFn | null`) is read and written through `any` the same way: the getter
//! converts the field's value to the interface field's type, checked at each read, and the
//! setter the value written to the field's.

use crate::any::Dynamic;
use crate::collect::getters::{value_getter, value_setter};
use crate::ctx::Ctx;
use crate::defs::{IfaceMethod, ThrowSrc};
use crate::hir::{
    self, Callee, Def, DefId, ExprKind as H, Intrinsic, LocalId, StmtKind, TyId, TyKind, UseMode,
};

impl Ctx<'_> {
    /// Can a `Function` field fill method `m` (whose interface arguments and implementor are
    /// `full`) through `callOn` (module docs)?
    pub(crate) fn call_on_fills(&mut self, m: &IfaceMethod, full: &[TyId]) -> bool {
        if self.prelude_fn(velt_common::std_hook!(call_on)).is_none() || self.any.is_none() {
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

    /// Does a field of type `from` convert to and from an interface field of type `to` through
    /// `any` (module docs)? Each is a type that goes into an `any` as it is, or an option of one.
    pub(crate) fn converts_via_any(&self, from: TyId, to: TyId) -> bool {
        let plain = |t: TyId| self.dynamic(t).is_some() || self.boxes_as_is(t);
        let core = |t: TyId| match self.ty.kind(t) {
            TyKind::Option(p, _) => *p,
            _ => t,
        };
        self.any.is_some() && plain(core(from)) && plain(core(to))
    }

    /// The getter of an interface field of type `want` reading field `index` (of type `ty`)
    /// through `any` (module docs).
    pub(crate) fn convert_getter(
        &mut self,
        d: DefId,
        self_ty: TyId,
        index: usize,
        ty: TyId,
        want: TyId,
    ) -> DefId {
        let fname = self.adt(d).expect("ICE: adt").fields[index].name.clone();
        let via = self.via_any_parts(ty, want);
        let copy = self.is_copy(ty);
        value_getter(self, d, self_ty, &fname, want, |span| {
            let this = hir::Expr {
                kind: H::Local(LocalId(0), UseMode::Borrow),
                ty: self_ty,
                span,
            };
            let read = hir::Expr {
                kind: H::Field {
                    base: Box::new(this),
                    index: index as u32,
                    mode: UseMode::Borrow,
                },
                ty,
                span,
            };
            via_any(read, copy, via, span)
        })
    }

    /// The getter of an interface field of type `want` reading field `index` (of type `ty`, an
    /// object type of the same layout): the same object (`Upcast`).
    pub(crate) fn layout_getter(
        &mut self,
        d: DefId,
        self_ty: TyId,
        index: usize,
        ty: TyId,
        want: TyId,
    ) -> DefId {
        let fname = self.adt(d).expect("ICE: adt").fields[index].name.clone();
        value_getter(self, d, self_ty, &fname, want, |span| {
            let this = hir::Expr {
                kind: H::Local(LocalId(0), UseMode::Borrow),
                ty: self_ty,
                span,
            };
            let read = hir::Expr {
                kind: H::Field {
                    base: Box::new(this),
                    index: index as u32,
                    mode: UseMode::Borrow,
                },
                ty,
                span,
            };
            hir::Expr {
                kind: H::Upcast(Box::new(intrinsic(Intrinsic::Share, read, ty, span))),
                ty: want,
                span,
            }
        })
    }

    /// The setter of an interface field of type `want` writing field `index` (of type `ty`)
    /// through `any` (module docs).
    pub(crate) fn convert_setter(
        &mut self,
        d: DefId,
        self_ty: TyId,
        index: usize,
        ty: TyId,
        want: TyId,
    ) -> DefId {
        let via = self.via_any_parts(want, ty);
        let copy = self.is_copy(want);
        value_setter(self, d, self_ty, index, want, |v, span| {
            via_any(set_borrow(v), copy, via, span)
        })
    }

    /// The getter of an optional interface field of a dynamic type `want` (`x?: any`) the
    /// object lacks: the dynamic `undefined`.
    pub(crate) fn absent_dynamic_getter(
        &mut self,
        d: DefId,
        self_ty: TyId,
        fname: &str,
        want: TyId,
    ) -> DefId {
        let nothing = self.dynamic_nothing_ty(hir::Nothing::Undefined);
        let via = self.via_any_parts(nothing, want);
        value_getter(self, d, self_ty, fname, want, |span| {
            let none = hir::Expr {
                kind: H::Lit(hir::Lit::Null),
                ty: nothing,
                span,
            };
            let boxed = hir::Expr {
                kind: H::ToDyn {
                    expr: Box::new(none),
                    impl_index: via.imp,
                },
                ty: via.any,
                span,
            };
            match boxed.ty == want {
                true => boxed,
                false => intrinsic(Intrinsic::AnyTo, boxed, want, span),
            }
        })
    }

    /// How a value of type `from` becomes a `to` through `any` ([`via_any`]).
    fn via_any_parts(&mut self, from: TyId, to: TyId) -> ViaAny {
        let defs = self.any.expect("ICE: any");
        let option = match *self.ty.kind(from) {
            TyKind::Option(p, n) => Some((p, self.is_copy(p), self.dynamic_nothing_ty(n))),
            _ => None,
        };
        ViaAny {
            any: self.ty.intern(TyKind::Dyn(defs.any, vec![])),
            imp: self.any_impl().expect("ICE: any"),
            boxed: self.dynamic(from).is_some(),
            option,
            to,
        }
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
        let (Some(call_on), Some(any_impl)) = (
            self.prelude_fn(velt_common::std_hook!(call_on)),
            self.any_impl(),
        ) else {
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

/// The parts of a conversion through `any` (`Ctx::via_any_parts`): the type `any`, its
/// implementation, whether the value is dynamic already, for an option its payload type (and
/// whether it is copied) and the type of the dynamic nothing, the target type.
#[derive(Clone, Copy)]
struct ViaAny {
    any: TyId,
    imp: u32,
    boxed: bool,
    option: Option<(TyId, bool, TyId)>,
    to: TyId,
}

/// The place `e` read as a borrow.
fn set_borrow(mut e: hir::Expr) -> hir::Expr {
    match &mut e.kind {
        H::Local(_, m) | H::Field { mode: m, .. } => *m = UseMode::Borrow,
        _ => {}
    }
    e
}

/// The value at the borrowed place `e` (of a type copied when `copy`) converted to `via.to`
/// through `any`: boxed (unless it is dynamic; an option's payload, or the dynamic nothing),
/// then checked as a `to`.
fn via_any(e: hir::Expr, copy: bool, via: ViaAny, span: velt_common::Span) -> hir::Expr {
    let to_dyn = |e: hir::Expr| hir::Expr {
        kind: H::ToDyn {
            expr: Box::new(e),
            impl_index: via.imp,
        },
        ty: via.any,
        span,
    };
    let value = |e: hir::Expr, copy: bool| match copy {
        true => match e.kind {
            H::Local(l, _) => hir::Expr {
                kind: H::Local(l, UseMode::Copy),
                ..e
            },
            H::Field { base, index, .. } => hir::Expr {
                kind: H::Field {
                    base,
                    index,
                    mode: UseMode::Copy,
                },
                ..e
            },
            kind => hir::Expr { kind, ..e },
        },
        false => {
            let ty = e.ty;
            intrinsic(Intrinsic::Share, e, ty, span)
        }
    };
    let boxed = match (via.boxed, via.option) {
        (true, _) => value(e, copy),
        (false, None) => to_dyn(value(e, copy)),
        (false, Some((payload, copy, nothing))) => {
            let some = hir::Expr {
                kind: H::UnwrapSome(
                    Box::new(e.clone()),
                    if copy { UseMode::Copy } else { UseMode::Borrow },
                ),
                ty: payload,
                span,
            };
            let some = match copy {
                true => some,
                false => intrinsic(Intrinsic::Share, some, payload, span),
            };
            let none = hir::Expr {
                kind: H::Lit(hir::Lit::Null),
                ty: nothing,
                span,
            };
            let pat = |kind| hir::Pat {
                kind,
                ty: e.ty,
                span,
            };
            let arms = vec![
                hir::Arm {
                    pat: pat(hir::PatKind::None),
                    guard: None,
                    body: to_dyn(none),
                },
                hir::Arm {
                    pat: pat(hir::PatKind::Wildcard),
                    guard: None,
                    body: to_dyn(some),
                },
            ];
            hir::Expr {
                kind: H::Match {
                    scrutinee: Box::new(e),
                    arms,
                },
                ty: via.any,
                span,
            }
        }
    };
    if boxed.ty == via.to {
        return boxed;
    }
    intrinsic(Intrinsic::AnyTo, boxed, via.to, span)
}

fn intrinsic(i: Intrinsic, arg: hir::Expr, ty: TyId, span: velt_common::Span) -> hir::Expr {
    hir::Expr {
        kind: H::Call {
            callee: Callee::Intrinsic(i),
            args: vec![arg],
        },
        ty,
        span,
    }
}
