//! A number literal's suffix says its type (`1i64`, `0.5f32`), which a decorator's parameter must
//! accept as a call's would: `@small(1i64)` for an `i8` parameter is refused even though the value
//! fits, since `small(1i64)` would be. [`super::consts`] folds the value; this finds the literals
//! whose suffix the parameter's type doesn't accept, through arrays, objects and `const`s.

use velt_common::Span;
use velt_syntax::ast;

use super::consts::constant_named;
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{FloatTy, IntTy, LitValue, TyId, TyKind};

/// How many `const`s a search follows: as many as the folder does (a cycle is reported there).
const MAX_DEPTH: u32 = 64;

/// A literal whose suffix its place in a parameter's type doesn't accept.
pub(super) struct WrongSuffix {
    /// The literal, or the name of the `const` in the argument that holds it.
    pub span: Span,
    pub suffix: String,
    /// The type of the place the literal is in.
    pub want: TyId,
}

type Found = Option<WrongSuffix>;

/// The first literal in `e` (written in module `m`) whose suffix a value of type `ty` can't have.
pub(super) fn wrong_suffix(cx: &Ctx, m: usize, e: &ast::Expr, ty: TyId) -> Found {
    search(cx, m, e, ty, 0)
}

/// The number type a suffix names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Num {
    Int(IntTy),
    Float(FloatTy),
}

fn suffix_type(s: &str) -> Option<Num> {
    Some(match s {
        "i8" => Num::Int(IntTy::I8),
        "i16" => Num::Int(IntTy::I16),
        "i32" => Num::Int(IntTy::I32),
        "i64" => Num::Int(IntTy::I64),
        "isize" => Num::Int(IntTy::ISize),
        "u8" => Num::Int(IntTy::U8),
        "u16" => Num::Int(IntTy::U16),
        "u32" => Num::Int(IntTy::U32),
        "u64" => Num::Int(IntTy::U64),
        "usize" => Num::Int(IntTy::USize),
        "f32" => Num::Float(FloatTy::F32),
        "f16" => Num::Float(FloatTy::F16),
        "bf16" => Num::Float(FloatTy::BF16),
        "f64" => Num::Float(FloatTy::F64),
        _ => return None,
    })
}

/// Does type `ty` accept a number of type `n`: that type, a literal type of it, or a union or an
/// optional type with one of those in it?
fn accepts(cx: &Ctx, ty: TyId, n: Num) -> bool {
    if cx.dynamic(ty).is_some() {
        return true;
    }
    match cx.ty.kind(ty) {
        TyKind::Error => true,
        TyKind::Int(t) => n == Num::Int(*t),
        TyKind::Float(t) => n == Num::Float(*t),
        TyKind::Literal(LitValue::Int(t, _)) => n == Num::Int(*t),
        TyKind::Literal(LitValue::Float(t, _)) => n == Num::Float(*t),
        TyKind::Option(inner, _) => accepts(cx, *inner, n),
        TyKind::Adt(d, _) => match &cx.info[d.0 as usize] {
            DefInfo::Enum(e) if e.is_union => {
                (e.variants.iter()).any(|v| v.payload.iter().any(|t| accepts(cx, *t, n)))
            }
            _ => false,
        },
        _ => false,
    }
}

/// The types a part of a value of type `ty` may have: `ty` itself, or each member of a union.
fn alternatives(cx: &Ctx, ty: TyId) -> Vec<TyId> {
    match cx.ty.kind(ty) {
        TyKind::Option(inner, _) => alternatives(cx, *inner),
        TyKind::Adt(d, _) => match &cx.info[d.0 as usize] {
            DefInfo::Enum(e) if e.is_union => (e.variants.iter())
                .flat_map(|v| v.payload.iter().copied())
                .collect(),
            _ => vec![ty],
        },
        _ => vec![ty],
    }
}

fn search(cx: &Ctx, m: usize, e: &ast::Expr, ty: TyId, depth: u32) -> Found {
    use ast::ExprKind as E;
    if depth > MAX_DEPTH {
        return None;
    }
    match &e.kind {
        E::Lit(ast::Lit::Int { suffix, .. } | ast::Lit::Float { suffix, .. }) => {
            let s = suffix.as_deref()?;
            let n = suffix_type(s)?;
            (!accepts(cx, ty, n)).then(|| WrongSuffix {
                span: e.span,
                suffix: s.to_string(),
                want: ty,
            })
        }
        E::Paren(inner)
        | E::Unary {
            op: ast::UnaryOp::Neg,
            expr: inner,
        }
        | E::Cast { expr: inner, .. } => search(cx, m, inner, ty, depth),
        // Packed arrays hold numbers without suffixes.
        E::NumArray(_) => None,
        E::Array(items) => {
            // The first alternative the elements all fit decides; none: the first one's answer.
            let mut first = None;
            for alt in alternatives(cx, ty) {
                let found = match cx.ty.kind(alt) {
                    TyKind::Array(elem) => {
                        (items.iter()).find_map(|x| search(cx, m, x, *elem, depth))
                    }
                    TyKind::Tuple(elems) => {
                        (items.iter().zip(elems)).find_map(|(x, t)| search(cx, m, x, *t, depth))
                    }
                    _ => continue,
                };
                match found {
                    None => return None,
                    Some(f) => {
                        first.get_or_insert(f);
                    }
                }
            }
            first
        }
        E::Object(props) => {
            let mut first = None;
            for alt in alternatives(cx, ty) {
                let TyKind::Adt(d, _) = cx.ty.kind(alt) else {
                    continue;
                };
                let DefInfo::Adt(a) = &cx.info[d.0 as usize] else {
                    continue;
                };
                let field_ty = |name: &str| a.fields.iter().find(|f| f.name == name).map(|f| f.ty);
                let found = props.iter().find_map(|p| match p {
                    ast::ObjectProp::KeyValue(k, v) => search(cx, m, v, field_ty(&k.name)?, depth),
                    ast::ObjectProp::Shorthand(k) => {
                        let path = std::slice::from_ref(k);
                        named(cx, m, path, field_ty(&k.name)?, depth)
                    }
                    _ => None,
                });
                match found {
                    None => return None,
                    Some(f) => {
                        first.get_or_insert(f);
                    }
                }
            }
            first
        }
        E::Ident(_) | E::Member { .. } => {
            let path = super::consts::path_of(e)?;
            // Reported where the argument names the constant.
            named(cx, m, &path, ty, depth).map(|w| WrongSuffix { span: e.span, ..w })
        }
        _ => None,
    }
}

/// [`search`] in the initializer of the `const` that `path` names.
fn named(cx: &Ctx, m: usize, path: &[ast::Ident], ty: TyId, depth: u32) -> Found {
    let d = constant_named(cx, m, path)?;
    let DefInfo::Global(g) = &cx.info[d.0 as usize] else {
        return None;
    };
    search(cx, g.module, g.src.init?, ty, depth + 1)
}
