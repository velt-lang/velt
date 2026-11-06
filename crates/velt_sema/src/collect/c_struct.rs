//! C structs (`declare c struct S { x: f64; y: f64; }`, docs/internals/design/c-ffi.md "C
//! structs"): fields in C's order, each of a type C has, and nothing else. In Velt a C struct is
//! a value: Copy (`copy_types.rs`), its fields read-only, built from a literal of its type,
//! never counted or boxed (`hir::AdtDef::c_struct`), and compared by content. What every struct
//! can't have (`extends`, a constructor) is reported as for them.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::ItemDefs;
use crate::ctx::Ctx;
use crate::hir::{DefId, FloatTy, Nothing, TyId, TyKind};

/// What a C struct's field may be, for the errors.
const FIELD_TYPES: &str = "a C struct's fields are integers (`i8` … `u64`, `isize`, `usize`), \
                           `f32`, `f64`, `bool` and other C structs";

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
        // An unknown type anywhere in it (`[Nope, Nope]`, `Map<Nope, i32>`) is reported already.
        if cx.ty.has_error(ty) {
            continue;
        }
        let shown = cx.display(ty);
        let note = match field_ty(cx, ty) {
            Field::Ok => continue,
            Field::Pointer => {
                cx.error(
                    Diagnostic::error(
                        format!("field `{field}` of C struct `{name}` is a pointer (`{shown}`), which a C struct can't hold yet"),
                        at,
                    )
                    .with_note("a pointer field is stored as C's address, `NULL` as `null`, from a later layer of #2187; until then a C struct holds numbers, `bool` and other C structs"),
                );
                continue;
            }
            Field::Array => {
                cx.error(
                    Diagnostic::error(
                        format!("field `{field}` of C struct `{name}` is a C array (`{shown}`), which a C struct can't hold yet"),
                        at,
                    )
                    .with_note("C arrays in C structs come with fixed-size arrays (`T[N]`, #1866 area 4): a tuple's storage isn't fixed to C's; until then declare the elements as fields of their own"),
                );
                continue;
            }
            Field::Enum => "a Velt enum is an `i64`, and a C enum is usually an `int`: declare the field as the C enum's integer type (`i32`), which the header check holds to the enum's width",
            Field::CType => "a `declare c type` exists only behind a pointer: a `Ptr<T>` field comes in a later layer of #2187",
            Field::No => FIELD_TYPES,
        };
        cx.error(
            Diagnostic::error(
                format!(
                    "field `{field}` of C struct `{name}` has type `{shown}`, which C doesn't have"
                ),
                at,
            )
            .with_note(note),
        );
    }
}

/// Whether a C struct's field can be a `ty`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Ok,
    /// A C pointer (`Ptr<T>`, `Ptr<T> | null`): C's, but not yet held. `Ptr<T> | null` is a
    /// Velt option of the address, larger than C's pointer, so pointer fields wait for their
    /// own storage (the address, `NULL` as `null`).
    Pointer,
    /// A tuple of one C type, C's array: not yet held. Lowering may count a tuple type the
    /// program writes in place and copies (boxing/tuples.rs), which would make the field a
    /// pointer, so C arrays wait for fixed-size arrays.
    Array,
    /// A Velt enum, an `i64`.
    Enum,
    /// A `declare c type` by value.
    CType,
    No,
}

fn field_ty(cx: &Ctx, ty: TyId) -> Field {
    match cx.ty.kind(ty) {
        TyKind::Int(_) | TyKind::Float(FloatTy::F32 | FloatTy::F64) | TyKind::Bool => Field::Ok,
        TyKind::Option(inner, n) if *n != Nothing::Both && super::c_sig::is_c_ptr(cx, *inner) => {
            Field::Pointer
        }
        TyKind::Tuple(es) => match es.first() {
            Some(&e) if es.iter().all(|&x| x == e) && field_ty(cx, e) != Field::No => Field::Array,
            _ => Field::No,
        },
        TyKind::Adt(d, _) if matches!(cx.info[d.0 as usize], crate::defs::DefInfo::Enum(_)) => {
            Field::Enum
        }
        TyKind::Adt(d, _) if cx.adt(*d).is_some_and(|a| a.decl.is_some_and(|t| t.c_type)) => {
            Field::CType
        }
        _ if cx.is_c_struct_ty(ty) => Field::Ok,
        _ if super::c_sig::is_c_ptr(cx, ty) => Field::Pointer,
        _ => Field::No,
    }
}
