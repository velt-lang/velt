//! C structs (`declare c struct S { x: f64; y: f64; }`, docs/internals/design/c-ffi.md "C
//! structs"): fields in C's order, each of a type C has, and nothing else. In Velt a C struct is
//! a value: Copy (`copy_types.rs`), its fields read-only, built from a literal of its type and
//! never counted. What every struct can't have (`extends`, a constructor) is reported as for them.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::ItemDefs;
use crate::ctx::Ctx;
use crate::hir::{DefId, FloatTy, Nothing, TyId, TyKind};

/// What a C struct's field may be, for the errors.
const FIELD_TYPES: &str = "a C struct's fields are integers (`i8` … `u64`, `isize`, `usize`), \
                           `f32`, `f64`, `bool`, other C structs, and tuples of one of these as C \
                           arrays (`[f32, f32, f32]` for `float[3]`)";

impl Ctx<'_> {
    /// Is `d` a `declare c struct`?
    pub(crate) fn is_c_struct(&self, d: DefId) -> bool {
        self.adt(d)
            .is_some_and(|a| a.decl.is_some_and(|t| t.c_struct))
    }

    /// Is `t` a C struct type?
    pub(crate) fn is_c_struct_ty(&self, t: TyId) -> bool {
        matches!(self.ty.kind(t), TyKind::Adt(d, _) if self.is_c_struct(*d))
    }
}

/// Reports what each C struct among `items` can't have (fields resolved).
pub(super) fn check_all(cx: &mut Ctx, items: &ItemDefs) {
    for &d in items {
        let Some(decl) = cx.adt(d).and_then(|a| a.decl).filter(|t| t.c_struct) else {
            continue;
        };
        check(cx, d, decl);
    }
}

fn check(cx: &mut Ctx, d: DefId, decl: &ast::TypeDecl) {
    let name = decl.name.name.clone();
    let no =
        |what: &str| format!("C struct `{name}` can't have {what}: it is C's struct, fields only");
    if let Some(g) = decl.generics.first() {
        cx.err(no("type parameters"), g.name.span);
    }
    if let Some(t) = decl.implements.first() {
        cx.err(no("`implements`"), t.span);
    }
    if let Some(m) = decl.methods.first() {
        cx.error(
            Diagnostic::error(no("methods"), m.decl.sig.name.span).with_note(
                "write functions that take the struct, or wrap it in a class of the binding's API",
            ),
        );
    }
    for f in &decl.fields {
        let what = if f.is_static {
            Some("static fields")
        } else if f.optional {
            Some("optional fields (C has every field)")
        } else if f.default.is_some() {
            Some("field defaults (a value states every field)")
        } else if f.is_private {
            Some("private fields (C sees every field)")
        } else {
            None
        };
        if let Some(what) = what {
            cx.err(no(what), f.span);
        }
    }
    if decl.fields.is_empty() {
        cx.error(
            Diagnostic::error(format!("C struct `{name}` has no fields"), decl.name.span)
                .with_note("C has no empty struct; an opaque C type is `declare c type`"),
        );
    }
    let fields: Vec<(String, TyId, Span)> = cx
        .adt(d)
        .map(|a| {
            a.fields
                .iter()
                .map(|f| (f.name.clone(), f.declared, f.span))
                .collect()
        })
        .unwrap_or_default();
    for (field, ty, at) in fields {
        let shown = cx.display(ty);
        match field_ty(cx, ty) {
            _ if ty == cx.ty.error => continue,
            Field::Ok => continue,
            Field::Pointer => {
                cx.error(
                    Diagnostic::error(
                        format!("field `{field}` of C struct `{name}` is a pointer (`{shown}`), which a C struct can't hold yet"),
                        at,
                    )
                    .with_note("a pointer field is stored as C's address, `NULL` as `null`, from a later layer of #2187; until then a C struct holds numbers, `bool`, other C structs and C arrays of them"),
                );
                continue;
            }
            Field::No => {}
        }
        cx.error(
            Diagnostic::error(
                format!(
                    "field `{field}` of C struct `{name}` has type `{shown}`, which C doesn't have"
                ),
                at,
            )
            .with_note(FIELD_TYPES),
        );
    }
}

/// Whether a C struct's field can be a `ty`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Ok,
    /// A C pointer (`Ptr<T>`, `Ptr<T> | null`), or a C array of them: C's, but not yet held.
    /// `Ptr<T> | null` is a Velt option of the address, larger than C's pointer, so pointer
    /// fields wait for their own storage (the address, `NULL` as `null`).
    Pointer,
    No,
}

fn field_ty(cx: &Ctx, ty: TyId) -> Field {
    match cx.ty.kind(ty) {
        TyKind::Int(_) | TyKind::Float(FloatTy::F32 | FloatTy::F64) | TyKind::Bool => Field::Ok,
        TyKind::Option(inner, n) if *n != Nothing::Both && super::c_sig::is_c_ptr(cx, *inner) => {
            Field::Pointer
        }
        // A C array: one element type, at least one element.
        TyKind::Tuple(es) => match es.first() {
            Some(&e) if es.iter().all(|&x| x == e) => field_ty(cx, e),
            _ => Field::No,
        },
        _ if cx.is_c_struct_ty(ty) => Field::Ok,
        _ if super::c_sig::is_c_ptr(cx, ty) => Field::Pointer,
        _ => Field::No,
    }
}
