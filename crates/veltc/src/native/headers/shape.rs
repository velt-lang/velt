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
        // A C struct by value (#2187 layer 3): compared by the definition it names.
        CTy::Struct => field_shape(p, t),
        CTy::Null => Shape::Null,
        CTy::StrBytes | CTy::CString => Shape::Text,
        CTy::Ptr { .. } => Shape::Ptr(pointee_of_ptr(p, t)),
        CTy::Buf { readonly } => match p.types.kind(t) {
            // An array of C structs (#2187): compared by the definition its elements name.
            &TyKind::Array(e) if scalar_of(p, e).is_none() => match field_shape(p, e) {
                Shape::Struct(name) => Shape::StructBuf {
                    name,
                    readonly,
                    const_only: readonly && c_unwritable(p, e, 0),
                },
                _ => Shape::Buf {
                    elem: Scalar::Bool,
                    readonly,
                },
            },
            TyKind::Array(e) => Shape::Buf {
                elem: scalar_of(p, *e).unwrap_or(Scalar::Bool),
                readonly,
            },
            _ => Shape::Buf {
                elem: Scalar::Bool,
                readonly,
            },
        },
        // `Out<T>`: a `T *` C writes; `Out<Ptr<T> | null>` is a `T **` (#3094); `Out<S>` of a C
        // struct an `S *` (#2187).
        // `Ref<S>`: a `const S *` C reads (#2187), `t` being `S` (or `S | null`).
        CTy::Ref { nullable } => match field_shape(p, option_inner(p, t)) {
            Shape::Struct(name) => Shape::RefStruct { name, nullable },
            other => other,
        },
        CTy::Out => match first_arg(p, t) {
            Some(a) if ptr_target(p, a).is_some() => Shape::OutPtr(pointee_of_ptr(p, a)),
            Some(a) => match field_shape(p, a) {
                Shape::Struct(name) => Shape::OutStruct(name),
                _ => Shape::Out(scalar_of(p, a).unwrap_or(Scalar::Bool)),
            },
            None => Shape::Out(Scalar::Bool),
        },
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

/// Does C struct `t` (`depth` structs deep) have a field C writing could make no Velt value: a
/// `bool` (another byte) or a non-null pointer (`NULL`)? As sema's rule for an array C writes
/// (velt_sema collect/c_sig.rs `c_unwritable`).
fn c_unwritable(p: &hir::Program, t: TyId, depth: u32) -> bool {
    match p.types.kind(t) {
        TyKind::Bool | TyKind::Ptr(_) => true,
        TyKind::Adt(d, _) if depth < 32 => match p.def(*d) {
            Def::Adt(a) if a.c_struct => {
                (a.fields.iter()).any(|f| c_unwritable(p, f.ty, depth + 1))
            }
            _ => false,
        },
        _ => false,
    }
}

/// The `T` of `T | null` `t`, or `t`.
fn option_inner(p: &hir::Program, t: TyId) -> TyId {
    match p.types.kind(t) {
        TyKind::Option(inner, _) => *inner,
        _ => t,
    }
}

/// The type argument of `Out<T>` `t`.
fn first_arg(p: &hir::Program, t: TyId) -> Option<TyId> {
    match p.types.kind(t) {
        TyKind::Adt(_, args) => args.first().copied(),
        _ => None,
    }
}

/// The `T` of `Ptr<T>` or `Ptr<T> | null` `t`, if it is one.
fn ptr_target(p: &hir::Program, t: TyId) -> Option<TyId> {
    match p.types.kind(t) {
        TyKind::Option(inner, _) => ptr_target(p, *inner),
        TyKind::Ptr(a) => Some(*a),
        _ => None,
    }
}

/// The pointee of `Ptr<T>` (or `Ptr<T> | null`) `t`.
fn pointee_of_ptr(p: &hir::Program, t: TyId) -> Pointee {
    match ptr_target(p, t) {
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
        TyKind::Ptr(_) => Pointee::Ptr(Box::new(pointee_of_ptr(p, t))),
        TyKind::Adt(d, _) => match p.def(*d) {
            Def::Adt(a) => Pointee::CType(a.name.rsplit("::").next().unwrap_or(&a.name).into()),
            _ => Pointee::Void,
        },
        TyKind::Option(inner, _) => pointee(p, *inner),
        _ => Pointee::Void,
    }
}

/// What a field of type `t` of a C struct holds: a number, `bool`, a pointer or another C
/// struct (sema allowed only these).
pub(super) fn field_shape(p: &hir::Program, t: TyId) -> Shape {
    if let Some(s) = scalar_of(p, t) {
        return Shape::Scalar(s);
    }
    match p.types.kind(t) {
        TyKind::Adt(d, _) => match p.def(*d) {
            Def::Adt(a) => Shape::Struct(short_name(&a.name)),
            _ => Shape::Void,
        },
        _ => Shape::Ptr(pointee_of_ptr(p, t)),
    }
}

/// A declaration's name without its module (`app::geom::Point` → `Point`): C's name.
pub(super) fn short_name(name: &str) -> String {
    name.rsplit("::").next().unwrap_or(name).to_string()
}
