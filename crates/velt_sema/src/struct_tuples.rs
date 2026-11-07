//! A declared `struct` (#641) can't hold a tuple yet, as a C struct can't (collect/c_struct.rs).
//! A struct is a value that copies inline and whose fields are read-only, but a tuple is stored
//! as lowering decides per tuple type (boxing/tuples.rs: counted once the program writes and
//! copies it), and its elements stay writable. A tuple field would make the struct's layout
//! depend on code elsewhere, let a write through `s.t[0]` reach every copy, and let a copy share
//! a counted tuple it doesn't count. Fixed-size arrays (`T[N]`, #1866 area 4) will hold such
//! elements inline. Like `void_fields`, this checks every instance the program's types mention,
//! after all bodies, so a generic struct is checked with the tuple it is instantiated with; a
//! struct holding another holds what that one holds, and the inner one is reported.

use rustc_hash::FxHashSet;
use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{AdtKind, DefId, TyId, TyKind};

pub(crate) fn check_instantiations(cx: &mut Ctx) {
    let mut reported: FxHashSet<(DefId, String)> = FxHashSet::default();
    let n = cx.ty.table.len() as u32;
    for i in 0..n {
        let t = TyId(i);
        let TyKind::Adt(d, args) = cx.ty.kind(t).clone() else {
            continue;
        };
        let Some(a) = cx.adt(d) else { continue };
        let value = a.kind == AdtKind::Struct
            && a.decl.is_some_and(|decl| !decl.c_struct)
            && !cx.place_struct(d);
        if !value {
            continue;
        }
        let name = a
            .decl
            .map(|decl| decl.name.name.clone())
            .unwrap_or_default();
        let def_span = a.span;
        let fields: Vec<(String, TyId, Span)> = a
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty, f.span))
            .collect();
        for (field, fty, field_span) in fields {
            if cx.ty.has_error(fty) || reported.contains(&(d, field.clone())) {
                continue;
            }
            let ty = cx.subst(fty, &args);
            if !holds_tuple(cx, ty) {
                continue;
            }
            reported.insert((d, field.clone()));
            let span = match field_span {
                Span::DUMMY => def_span,
                s => s,
            };
            let shown = cx.display(ty);
            let instance = match args.is_empty() {
                true => String::new(),
                false => format!(" (in `{}`)", cx.display(t)),
            };
            cx.error(
                Diagnostic::error(
                    format!("field `{field}` of struct `{name}` holds a tuple (`{shown}`){instance}, which a struct can't hold yet"),
                    span,
                )
                .with_note("a struct is a value copied inline with read-only fields, and a tuple's storage and elements are neither; until fixed-size arrays (`T[N]`) declare the elements as fields of their own, or make the type a class"),
            );
        }
    }
}

/// Is `ty` a tuple, or a tuple or null?
fn holds_tuple(cx: &Ctx, ty: TyId) -> bool {
    match cx.ty.kind(ty) {
        TyKind::Tuple(_) => true,
        TyKind::Option(x, _) => holds_tuple(cx, *x),
        _ => false,
    }
}
