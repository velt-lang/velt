//! A declared `struct` (#641) can't hold a tuple yet, as a C struct can't (collect/c_struct.rs).
//! A struct is a value that copies inline and whose fields are read-only, but a tuple is stored
//! as lowering decides per tuple type (boxing/tuples.rs: counted once the program writes and
//! copies it), and its elements stay writable. A tuple field would make the struct's layout
//! depend on code elsewhere, let a write through `s.t[0]` reach every copy, and let a copy share
//! a counted tuple it doesn't count. Fixed-size arrays (`T[N]`, #1866 area 4) will hold such
//! elements inline. Like `void_fields`, this checks every instance the program's types mention,
//! after all bodies, so a generic struct is checked with the tuple it is instantiated with; a
//! struct holding another holds what that one holds, and the inner one is reported. An instance
//! made only inside a generic body (`G<T>` in `outer<T>`) is checked where the body is
//! instantiated: [`check_generic_bodies`] carries "this parameter lands in a struct field" from
//! each generic function to its callers, as `record_keys` carries key requirements.

use rustc_hash::{FxHashMap, FxHashSet};
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
            if !holds_tuple(cx, ty, 0) {
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

/// Is `ty` a tuple, or a tuple or null, or a union with a tuple member?
fn holds_tuple(cx: &mut Ctx, ty: TyId, depth: u32) -> bool {
    if depth > 8 {
        return false;
    }
    match cx.ty.kind(ty).clone() {
        TyKind::Tuple(_) => true,
        TyKind::Option(x, _) => holds_tuple(cx, x, depth + 1),
        _ => match cx.union_members(ty) {
            Some(ms) => ms.into_iter().any(|m| holds_tuple(cx, m, depth + 1)),
            None => false,
        },
    }
}

/// The type parameters that `ty`, mentioned in a generic body, would store in a struct field:
/// a field of a struct instance in it that is the parameter, the parameter or null, or a union
/// with it as a member.
fn params_in_fields(cx: &mut Ctx, ty: TyId, out: &mut FxHashSet<u32>, depth: u32) {
    if depth > 8 {
        return;
    }
    match cx.ty.kind(ty).clone() {
        TyKind::Adt(d, args) => {
            let value = cx.adt(d).is_some_and(|a| {
                a.kind == AdtKind::Struct && a.decl.is_some_and(|decl| !decl.c_struct)
            }) && !cx.place_struct(d);
            if value {
                let fields: Vec<TyId> = cx
                    .adt(d)
                    .map(|a| a.fields.iter().map(|f| f.ty).collect())
                    .unwrap_or_default();
                for f in fields {
                    let s = cx.subst(f, &args);
                    field_params(cx, s, out, 0);
                    params_in_fields(cx, s, out, depth + 1);
                }
            }
            for a in args {
                params_in_fields(cx, a, out, depth + 1);
            }
        }
        TyKind::Array(x) | TyKind::Option(x, _) => params_in_fields(cx, x, out, depth + 1),
        _ => {}
    }
}

/// The parameters a field of type `ty` holds as itself, or null, or a union member.
fn field_params(cx: &mut Ctx, ty: TyId, out: &mut FxHashSet<u32>, depth: u32) {
    if depth > 8 {
        return;
    }
    match cx.ty.kind(ty).clone() {
        TyKind::Param(i) => {
            out.insert(i);
        }
        TyKind::Option(x, _) => field_params(cx, x, out, depth + 1),
        _ => {
            if let Some(ms) = cx.union_members(ty) {
                for m in ms {
                    field_params(cx, m, out, depth + 1);
                }
            }
        }
    }
}

/// A tuple given for a type parameter that a generic body stores in a struct field, through any
/// chain of generic calls, is an error at the call that gives it.
/// `uses`: the instantiation sites `record_keys` collected (`inst_sites::collect`).
pub(crate) fn check_generic_bodies(cx: &mut Ctx, uses: &[(DefId, crate::inst_sites::Uses)]) {
    let mut needs: FxHashMap<DefId, FxHashSet<u32>> = FxHashMap::default();
    for (f, u) in uses {
        let mut set = FxHashSet::default();
        for (t, _) in &u.types {
            params_in_fields(cx, *t, &mut set, 0);
        }
        if !set.is_empty() {
            needs.entry(*f).or_default().extend(set);
        }
    }
    let mut reported: FxHashSet<(DefId, Span)> = FxHashSet::default();
    loop {
        let mut changed = false;
        for (f, u) in uses {
            let mut add: FxHashSet<u32> = FxHashSet::default();
            for c in &u.closures {
                if let Some(n) = needs.get(c) {
                    add.extend(n.iter().copied());
                }
            }
            for (g, targs, span) in &u.calls {
                let Some(n) = needs.get(g).cloned() else {
                    continue;
                };
                for i in n {
                    let Some(&a) = targs.get(i as usize) else {
                        continue;
                    };
                    if holds_tuple(cx, a, 0) {
                        if reported.insert((*g, *span)) {
                            let shown = cx.display(a);
                            cx.error(
                                Diagnostic::error(
                                    format!("this call stores a tuple (`{shown}`) in a struct's field, which a struct can't hold yet"),
                                    *span,
                                )
                                .with_note("the function builds a struct whose field has this type argument's type; until fixed-size arrays (`T[N]`) pass the elements as fields of a struct of their own, or use a class"),
                            );
                        }
                    } else {
                        field_params(cx, a, &mut add, 0);
                    }
                }
            }
            let entry = needs.entry(*f).or_default();
            for i in add {
                changed |= entry.insert(i);
            }
        }
        if !changed {
            break;
        }
    }
}
