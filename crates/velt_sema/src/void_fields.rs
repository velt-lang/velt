//! A generic struct or class instantiated so that a field has type `void` (e.g. `IoResult<void>`
//! for `struct IoResult<T> { ...; value: T }`) has no layout: fields are values, `void` is not.
//! Declared `void` fields are rejected where they are declared (`collect::shapes`); this pass
//! checks every instantiation the program's types mention, after all bodies.

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

pub(crate) fn check_instantiations(cx: &mut Ctx) {
    let n = cx.ty.table.len() as u32;
    for i in 0..n {
        let t = TyId(i);
        let TyKind::Adt(d, args) = cx.ty.kind(t).clone() else {
            continue;
        };
        if args.is_empty() || !args.contains(&cx.ty.unit) {
            continue;
        }
        let Some(a) = cx.adt(d) else { continue };
        let fields: Vec<(String, TyId)> = a.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
        let span = a.span;
        for (name, fty) in fields {
            if cx.subst(fty, &args) == cx.ty.unit {
                let tn = cx.display(t);
                cx.error(
                    Diagnostic::error(
                        format!("`{tn}` would have a field `{name}` of type `void`"),
                        span,
                    )
                    .with_note(
                        "fields must be values; use a type without that field for the `void` case",
                    ),
                );
                break;
            }
        }
    }
}
