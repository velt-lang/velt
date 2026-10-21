//! Interface fields are read through getter methods (hir.rs header): for every `implements`
//! pair and interface field, sema synthesizes a method `C.<field>` returning the field (a copy
//! for Copy types, else a clone) and appends it to the impl's method slots, after the
//! interface's declared methods. `x.field` on a `T extends I` / interface-typed receiver is then
//! a `ParamMethod` / `Dyn` call of slot `methods.len() + field index`.
//!
//! An interface the program writes a field through also gets setters ([`field_setter`]), one
//! per field, after the getters (`crate::iface_writes`).

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind, ThisSig};
use crate::hir::{
    self, Callee, Def, DefId, ExprKind as H, Intrinsic, LocalDef, LocalId, PassMode, TyId, UseMode,
};

/// The getter of field `index` (type `ty`) of class/struct `d` (self type `self_ty`).
pub(crate) fn field_getter(cx: &mut Ctx, d: DefId, self_ty: TyId, index: usize, ty: TyId) -> DefId {
    let a = cx.adt(d).expect("ICE: adt");
    let (qual, n, span, module) = (a.qual_name.clone(), a.generics.clone(), a.span, a.module);
    let fname = a.fields[index].name.clone();
    let name = format!("{qual}.<{fname}>");
    let mut info = super::fn_placeholder(name.clone(), span, span, module, FnKind::Method, None);
    info.generics = n.clone();
    info.this = Some(ThisSig {
        ty: self_ty,
        mode: PassMode::Borrow,
    });
    info.ret = ty;
    info.fixed_modes = true;
    info.state = BodyState::Done;
    info.owner = Some(d);
    info.local_kinds = vec![crate::body::LocalKind::This];
    let def = cx.alloc_def(span, DefInfo::Fn(Box::new(info)));
    let copy = cx.is_copy(ty);
    let this = hir::Expr {
        kind: H::Local(LocalId(0), UseMode::Borrow),
        ty: self_ty,
        span,
    };
    let field = |mode| hir::Expr {
        kind: H::Field {
            base: Box::new(this.clone()),
            index: index as u32,
            mode,
        },
        ty,
        span,
    };
    let value = if copy {
        field(UseMode::Copy)
    } else {
        hir::Expr {
            kind: H::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                args: vec![field(UseMode::Borrow)],
            },
            ty,
            span,
        }
    };
    cx.defs[def.0 as usize] = Some(Def::Fn(getter_def(
        name,
        n.len() as u32,
        self_ty,
        ty,
        value,
        span,
    )));
    def
}

/// The setter of field `index` (type `ty`) of class/struct `d` (self type `self_ty`):
/// `this.<field> = v`, with `v` owned (copied for Copy types) and moved into the field, as a
/// class setter takes it: the caller shares a value it uses again. The old value is released by
/// the assignment.
pub(crate) fn field_setter(cx: &mut Ctx, d: DefId, self_ty: TyId, index: usize, ty: TyId) -> DefId {
    let fname = cx.adt(d).expect("ICE: adt").fields[index].name.clone();
    let unit = cx.ty.unit;
    setter_def(cx, d, self_ty, &fname, ty, |span, copy| {
        let this = hir::Expr {
            kind: H::Local(LocalId(0), UseMode::BorrowMut),
            ty: self_ty,
            span,
        };
        let place = hir::Expr {
            kind: H::Field {
                base: Box::new(this),
                index: index as u32,
                mode: UseMode::BorrowMut,
            },
            ty,
            span,
        };
        let value = hir::Expr {
            kind: H::Local(LocalId(1), if copy { UseMode::Copy } else { UseMode::Move }),
            ty,
            span,
        };
        let assign = H::Assign {
            place: Box::new(place),
            value: Box::new(value),
        };
        Some((assign, unit))
    })
}

/// The setter slot of field `fname` (type `ty`) of class `d` that has a `get` accessor and no
/// `set` (written through an interface another implementation stores the field for): it
/// stops with Node's `TypeError`, as strict code does (`iface_writes`).
pub(crate) fn getter_only_setter(
    cx: &mut Ctx,
    d: DefId,
    self_ty: TyId,
    fname: &str,
    ty: TyId,
) -> DefId {
    let cname = cx.adt(d).expect("ICE: adt").name.clone();
    let msg =
        format!("TypeError: Cannot set property {fname} of #<{cname}> which has only a getter");
    let (str_, never) = (cx.ty.str_, cx.ty.never);
    setter_def(cx, d, self_ty, fname, ty, |span, _| {
        let call = H::Call {
            callee: Callee::Intrinsic(Intrinsic::Panic),
            args: vec![hir::Expr {
                kind: H::Lit(hir::Lit::Str(msg)),
                ty: str_,
                span,
            }],
        };
        Some((call, never))
    })
}

/// A setter slot of `readonly` field `fname` (type `ty`) for an implementation that cannot be
/// written through (an object literal's `get f()` without `set f(v)`): it does nothing, and is
/// never called (a write into a part of the field reports the literal,
/// `crate::iface_writes`).
pub(crate) fn unused_setter(cx: &mut Ctx, d: DefId, self_ty: TyId, fname: &str, ty: TyId) -> DefId {
    setter_def(cx, d, self_ty, fname, ty, |_, _| None)
}

/// A setter method `<set fname>(v: ty)` of class/struct `d` whose body is the statement
/// `body(span, ty is Copy)` (its kind and type), or empty for `None`.
fn setter_def(
    cx: &mut Ctx,
    d: DefId,
    self_ty: TyId,
    fname: &str,
    ty: TyId,
    body: impl FnOnce(Span, bool) -> Option<(H, TyId)>,
) -> DefId {
    let a = cx.adt(d).expect("ICE: adt");
    let (qual, n, span, module) = (a.qual_name.clone(), a.generics.clone(), a.span, a.module);
    let name = format!("{qual}.<set {fname}>");
    let copy = cx.is_copy(ty);
    let mode = if copy {
        PassMode::Copy
    } else {
        PassMode::Owned
    };
    let mut info = super::fn_placeholder(name.clone(), span, span, module, FnKind::Method, None);
    info.generics = n.clone();
    info.this = Some(ThisSig {
        ty: self_ty,
        mode: PassMode::BorrowMut,
    });
    info.params = vec![crate::defs::ParamSig {
        name: "v".into(),
        span,
        ty,
        mode,
        default: None,
    }];
    info.ret = cx.ty.unit;
    info.fixed_modes = true;
    info.state = BodyState::Done;
    info.owner = Some(d);
    info.local_kinds = vec![crate::body::LocalKind::This, crate::body::LocalKind::Param];
    let def = cx.alloc_def(span, DefInfo::Fn(Box::new(info)));
    let unit = cx.ty.unit;
    let stmts: Vec<hir::Stmt> = body(span, copy)
        .map(|(kind, ty_of)| hir::Stmt {
            kind: hir::StmtKind::Expr(hir::Expr {
                kind,
                ty: ty_of,
                span,
            }),
            span,
        })
        .into_iter()
        .collect();
    cx.defs[def.0 as usize] = Some(Def::Fn(hir::FnDef {
        name,
        generics: n.len() as u32,
        params: vec![
            hir::Param {
                local: LocalId(0),
                ty: self_ty,
                mode: PassMode::BorrowMut,
            },
            hir::Param {
                local: LocalId(1),
                ty,
                mode,
            },
        ],
        ret: unit,
        is_async: false,
        is_generator: false,
        self_ty: Some(self_ty),
        captures: vec![],
        shares_captures: false,
        body: hir::Body {
            locals: vec![local("this", self_ty, span), local("v", ty, span)],
            block: hir::Block {
                stmts,
                value: None,
                span,
            },
        },
        throws: None,
        span,
    }));
    def
}

fn local(name: &str, ty: TyId, span: Span) -> LocalDef {
    LocalDef {
        name: name.into(),
        ty,
        mutable: false,
        boxed: false,
        span,
    }
}

fn getter_def(
    name: String,
    generics: u32,
    self_ty: TyId,
    ret: TyId,
    value: hir::Expr,
    span: Span,
) -> hir::FnDef {
    hir::FnDef {
        name,
        generics,
        params: vec![hir::Param {
            local: LocalId(0),
            ty: self_ty,
            mode: PassMode::Borrow,
        }],
        ret,
        is_async: false,
        is_generator: false,
        self_ty: Some(self_ty),
        captures: vec![],
        shares_captures: false,
        body: hir::Body {
            locals: vec![local("this", self_ty, span)],
            block: hir::Block {
                stmts: vec![hir::Stmt {
                    kind: hir::StmtKind::Return(Some(value)),
                    span,
                }],
                value: None,
                span,
            },
        },
        throws: None,
        span,
    }
}
