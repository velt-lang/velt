//! What a `declare c` function's parameters and result pass, from their Velt types, for
//! [`super::compare`].

use velt_sema::hir::{self, CTy, Def, IntTy, TyId, TyKind};

use super::compare::{Pointee, Scalar, Shape};

/// What a parameter or result of type `t` (crossing as `c`) passes.
pub(super) fn shape(p: &hir::Program, t: TyId, c: CTy) -> Shape {
    match c {
        CTy::Int(i) => Shape::Scalar(int(i)),
        CTy::F32 => Shape::Scalar(Scalar::F32),
        CTy::F64 => Shape::Scalar(Scalar::F64),
        CTy::Bool => Shape::Scalar(Scalar::Bool),
        CTy::Void => Shape::Void,
        CTy::Null => Shape::Null,
        CTy::StrBytes | CTy::CString => Shape::Text,
        CTy::Ptr { .. } => Shape::Ptr(pointee_of_ptr(p, t)),
        CTy::Buf { readonly } => Shape::Buf {
            elem: match p.types.kind(t) {
                TyKind::Array(e) => scalar_of(p, *e).unwrap_or(Scalar::Bool),
                _ => Scalar::Bool,
            },
            readonly,
        },
        CTy::Out => Shape::Out(match first_arg(p, t) {
            Some(a) => scalar_of(p, a).unwrap_or(Scalar::Bool),
            None => Scalar::Bool,
        }),
        CTy::OutPtr => Shape::OutPtr(match first_arg(p, t) {
            Some(a) => pointee(p, a),
            None => Pointee::Void,
        }),
    }
}

fn int(i: IntTy) -> Scalar {
    let (bits, signed) = match i {
        IntTy::I8 => (8, true),
        IntTy::I16 => (16, true),
        IntTy::I32 => (32, true),
        IntTy::I64 | IntTy::ISize => (64, true),
        IntTy::U8 => (8, false),
        IntTy::U16 => (16, false),
        IntTy::U32 => (32, false),
        IntTy::U64 | IntTy::USize => (64, false),
    };
    Scalar::Int {
        bits,
        signed,
        pointer_sized: matches!(i, IntTy::ISize | IntTy::USize),
    }
}

fn scalar_of(p: &hir::Program, t: TyId) -> Option<Scalar> {
    match p.types.kind(t) {
        TyKind::Int(i) => Some(int(*i)),
        TyKind::Float(hir::FloatTy::F32) => Some(Scalar::F32),
        TyKind::Float(hir::FloatTy::F64) => Some(Scalar::F64),
        TyKind::Bool => Some(Scalar::Bool),
        _ => None,
    }
}

fn first_arg(p: &hir::Program, t: TyId) -> Option<TyId> {
    match p.types.kind(t) {
        TyKind::Option(inner, _) => first_arg(p, *inner),
        TyKind::Adt(_, args) => args.first().copied(),
        _ => None,
    }
}

/// The pointee of `Ptr<T>` (or `Ptr<T> | null`) `t`.
fn pointee_of_ptr(p: &hir::Program, t: TyId) -> Pointee {
    match first_arg(p, t) {
        Some(a) => pointee(p, a),
        None => Pointee::Void,
    }
}

/// What a pointer to `t` points at.
fn pointee(p: &hir::Program, t: TyId) -> Pointee {
    if let Some(s) = scalar_of(p, t) {
        return Pointee::Scalar(s);
    }
    match p.types.kind(t) {
        TyKind::Unit => Pointee::Void,
        TyKind::Adt(d, _) => match p.def(*d) {
            Def::Adt(a) if a.name == velt_common::std_item!(ffi_ptr, "Ptr") => {
                Pointee::Ptr(Box::new(pointee_of_ptr(p, t)))
            }
            Def::Adt(a) => Pointee::CType(a.name.rsplit("::").next().unwrap_or(&a.name).into()),
            _ => Pointee::Void,
        },
        TyKind::Option(inner, _) => pointee(p, *inner),
        _ => Pointee::Void,
    }
}
