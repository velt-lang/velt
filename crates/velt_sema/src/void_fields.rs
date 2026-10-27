//! A generic struct or class instantiated so that a field has type `void` (e.g. `IoResult<void>`
//! for `struct IoResult<T> { ...; value: T }`) has no layout: fields are values, `void` is not.
//! Declared `void` fields are rejected where they are declared (`collect::shapes`); this pass
//! checks every instantiation the program's types mention, after all bodies. An anonymous object
//! type is the exception: its canonical form holds `undefined` in such a field (`anon.rs`), as a
//! `void` field does in TypeScript (sigx's `__ref: TRef` brand at `TRef = void`).

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{AdtKind, TyId, TyKind};

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
        let anon = a.kind == AdtKind::Anon;
        let fields: Vec<(String, TyId, Span)> = a
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty, f.span))
            .collect();
        let def_span = a.span;
        // An anonymous object type's `void` field holds `undefined` in its canonical form
        // (`Ctx::canon`); only that form is a type values have.
        if anon && cx.canon(t) != t {
            continue;
        }
        for (name, fty, field_span) in fields {
            // At the field where it is written; an anonymous object type has no span of its
            // own (an intersection's fields are those of the parts it merges).
            let span = match field_span {
                Span::DUMMY => def_span,
                s => s,
            };
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
