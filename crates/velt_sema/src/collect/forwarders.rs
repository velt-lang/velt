//! Synthesized class methods that keep dispatch simple for lowering:
//! - a **forwarder** `C.m` calling interface `I`'s default `m` with `Self = C`, created when a
//!   subclass of `C` overrides the default `m` that `C` gets from `I` — so every class vtable
//!   entry is a class method;
//! - a **virtual trampoline** `C.<m>` calling `this.m(..)` through `C`'s vtable, used as the
//!   `ImplDef` entry when `m` has a vtable slot — so a subclass instance used as an interface
//!   value still runs its override;
//! - an **inherited default** `B.m` of interface `B extends A`, calling `A`'s default `m` (see
//!   [`iface_extends`](super::iface_extends)) — so every interface's defaults take exactly its
//!   own generics plus `Self`.

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind, Generics, ParamSig, ThisSig, ThrowSrc};
use crate::hir::{
    self, Callee, Def, DefId, ExprKind as H, LocalDef, LocalId, PassMode, TyId, UseMode,
};

/// What a synthesized method calls.
pub(super) enum Target {
    /// Interface default `def` with these type args (`iface args ++ [Self]`).
    Default(DefId, Vec<TyId>),
    /// Vtable slot of the receiver's class.
    Slot(u32),
}

/// The type a synthesized method belongs to.
pub(super) struct Host {
    pub d: DefId,
    pub generics: Generics,
    pub span: Span,
    pub module: usize,
}

impl Host {
    /// Class / struct `d`, with its own generics.
    pub fn adt(cx: &Ctx, d: DefId) -> Host {
        let a = cx.adt(d).expect("ICE: adt");
        Host {
            d,
            generics: a.generics.clone(),
            span: a.span,
            module: a.module,
        }
    }
}

/// A method of `host` (self type `self_ty`) with the given signature that calls `target`.
#[allow(clippy::too_many_arguments)] // the signature parts of the synthesized method
pub(super) fn synth_method(
    cx: &mut Ctx,
    host: &Host,
    self_ty: TyId,
    name: String,
    params: Vec<ParamSig>,
    ret: TyId,
    mut_this: bool,
    target: Target,
) -> DefId {
    let (d, generics, span, module) = (host.d, host.generics.clone(), host.span, host.module);
    let this_mode = if mut_this {
        PassMode::BorrowMut
    } else {
        PassMode::Borrow
    };
    let mut info = super::fn_placeholder(name.clone(), span, span, module, FnKind::Method, None);
    info.generics = generics.clone();
    info.this = Some(ThisSig {
        ty: self_ty,
        mode: this_mode,
    });
    info.params = params.clone();
    info.ret = ret;
    info.fixed_modes = true;
    info.state = BodyState::Done;
    info.owner = Some(d);
    info.local_kinds = std::iter::once(crate::body::LocalKind::This)
        .chain(params.iter().map(|_| crate::body::LocalKind::Param))
        .collect();
    let def = cx.alloc_def(span, DefInfo::Fn(Box::new(info)));
    let callee = match target {
        Target::Default(dd, targs) => {
            cx.fn_info_mut(def).throw_srcs = vec![ThrowSrc::Call(dd, targs.clone(), span)];
            Callee::Def(dd, targs)
        }
        Target::Slot(slot) => {
            // Vtable entries share one error type (a dispatch group), which has no type params.
            let entry = cx.adt(d).and_then(|a| a.vtable.get(slot as usize).copied());
            if let Some(m) = entry {
                cx.fn_info_mut(def).throw_srcs = vec![ThrowSrc::Call(m, vec![], span)];
            }
            Callee::Virtual { slot }
        }
    };
    let body = forward_body(cx, self_ty, this_mode, &params, ret, callee, span);
    let fndef = hir::FnDef {
        name,
        generics: generics.len() as u32,
        params: std::iter::once(hir::Param {
            local: LocalId(0),
            ty: self_ty,
            mode: this_mode,
        })
        .chain(params.iter().enumerate().map(|(i, p)| hir::Param {
            local: LocalId(i as u32 + 1),
            ty: p.ty,
            mode: p.mode,
        }))
        .collect(),
        ret,
        is_async: false,
        self_ty: Some(self_ty),
        captures: vec![],
        body,
        throws: None,
        span,
    };
    cx.defs[def.0 as usize] = Some(Def::Fn(fndef));
    def
}

/// `return callee(this, p1, .., pn);`
fn forward_body(
    cx: &mut Ctx,
    self_ty: TyId,
    this_mode: PassMode,
    params: &[ParamSig],
    ret: TyId,
    callee: Callee,
    span: Span,
) -> hir::Body {
    let local = |name: &str, ty| LocalDef {
        name: name.to_string(),
        ty,
        mutable: false,
        boxed: false,
        span,
    };
    let mut locals = vec![local("this", self_ty)];
    let this_use = if this_mode == PassMode::BorrowMut {
        UseMode::BorrowMut
    } else {
        UseMode::Borrow
    };
    let mut args = vec![hir::Expr {
        kind: H::Local(LocalId(0), this_use),
        ty: self_ty,
        span,
    }];
    for (i, p) in params.iter().enumerate() {
        locals.push(local(&p.name, p.ty));
        let mode = if cx.is_copy(p.ty) {
            UseMode::Copy
        } else {
            UseMode::Borrow
        };
        args.push(hir::Expr {
            kind: H::Local(LocalId(i as u32 + 1), mode),
            ty: p.ty,
            span,
        });
    }
    let call = hir::Expr {
        kind: H::Call { callee, args },
        ty: ret,
        span,
    };
    let kind = if ret == cx.ty.unit {
        hir::StmtKind::Expr(call)
    } else {
        hir::StmtKind::Return(Some(call))
    };
    hir::Body {
        locals,
        block: hir::Block {
            stmts: vec![hir::Stmt { kind, span }],
            value: None,
            span,
        },
    }
}
