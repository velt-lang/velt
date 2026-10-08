//! Interface fields are read through getter methods (hir.rs header): for every `implements`
//! pair and interface field, sema synthesizes a method `C.<field>` returning the field (a copy
//! for Copy types, else a clone) and appends it to the impl's method slots, after the
//! interface's declared methods. `x.field` on a `T extends I` / interface-typed receiver is then
//! a `ParamMethod` / `Dyn` call of slot `methods.len() + field index`.

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
            locals: vec![LocalDef {
                name: "this".into(),
                ty: self_ty,
                mutable: false,
                boxed: false,
                span,
            }],
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
