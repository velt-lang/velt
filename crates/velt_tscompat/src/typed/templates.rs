//! Rules on turning values into text: template literals of objects (`object-in-template`) and
//! of values that may be `null` (`nullable-in-template`), and `JSON.stringify` of a `Map`
//! (`json-map`; a `Set` has no JSON form in Velt).

use std::collections::HashSet;

use velt_sema::ide::{NamedKind, TypeRef, TypeView};
use velt_syntax::ast::{self, ExprKind as E};

use super::numbers::is_global;
use super::{nulls, Typed};
use crate::Severity;

/// How a template literal prints a value, compared with JavaScript.
enum Printed {
    /// The same in both (numbers, strings, booleans, enums, a class's own `toString()`), or
    /// unknown (a generic parameter).
    Same,
    /// The same, except that JavaScript prints `undefined` where the value is `undefined`.
    Nullable,
    /// Velt prints the contents; JavaScript `[object Object]` (also for the elements of an array).
    Contents,
}

/// `${x}` in a template literal.
pub(super) fn template(exprs: &[ast::Expr], t: &mut Typed) {
    for e in exprs {
        let Some(ty) = t.type_of(e) else { continue };
        match printed(&ty, t, 0) {
            Printed::Same => {}
            // Only a value that is `undefined` in JavaScript prints differently: a `null`
            // prints `null` in both.
            Printed::Nullable => {
                let Some(why) = nulls::undefined_source(e, t) else {
                    continue;
                };
                t.cx.report(
                    "nullable-in-template",
                    Severity::Warning,
                    e.span,
                    "`${…}` of a value that may be `undefined` in JavaScript: it prints \
                     `undefined` there, `null` in Velt"
                        .into(),
                    &[
                        why,
                        "say what to print for nothing: `${x ?? \"\"}`, or test the value first",
                    ],
                    None,
                )
            }
            Printed::Contents => {
                let what = describe(&ty, t);
                t.cx.report(
                    "object-in-template",
                    Severity::Error,
                    e.span,
                    format!("`${{…}}` of {what} prints its contents in Velt, not in JavaScript"),
                    &[
                        "Velt formats an object as `console.log` does (`P { x: 1 }`), also as \
                         an element of an array; JavaScript calls `toString()`, which gives \
                         `[object Object]`",
                        "format it yourself: `xs.join(\", \")`, a field (`${p.name}`), or a \
                         `toString()` method the class declares itself, which both call",
                    ],
                    None,
                );
            }
        }
    }
}

/// `an array`, `an object`, `` a value of type `User` ``.
fn describe(ty: &TypeRef, t: &Typed) -> String {
    match t.view(ty) {
        TypeView::Nullable(inner) => describe(&inner, t),
        TypeView::Array(_) | TypeView::Tuple(_) => "an array".into(),
        TypeView::Map(..) => "a `Map`".into(),
        TypeView::Set(_) => "a `Set`".into(),
        TypeView::Record => "an object".into(),
        TypeView::Named(n) => format!("a value of type `{}`", n.name),
        TypeView::Promise(_) => "a `Promise`".into(),
        TypeView::Fn => "a function".into(),
        _ => "a value that may be an object".into(),
    }
}

fn printed(ty: &TypeRef, t: &Typed, depth: u32) -> Printed {
    if depth > 8 {
        return Printed::Same;
    }
    match t.view(ty) {
        TypeView::Nullable(inner) => match printed(&inner, t, depth + 1) {
            Printed::Same | Printed::Nullable => Printed::Nullable,
            Printed::Contents => Printed::Contents,
        },
        TypeView::Union(members) => {
            members
                .iter()
                .map(|m| printed(m, t, depth + 1))
                .fold(Printed::Same, |acc, p| match (acc, p) {
                    (Printed::Contents, _) | (_, Printed::Contents) => Printed::Contents,
                    (Printed::Nullable, _) | (_, Printed::Nullable) => Printed::Nullable,
                    _ => Printed::Same,
                })
        }
        // An array is written as JS writes it (`1,2`, #757) when its elements are.
        TypeView::Array(e) => element_printed(&e, t, depth),
        TypeView::Tuple(es) => es
            .iter()
            .map(|e| element_printed(e, t, depth))
            .find(|p| matches!(p, Printed::Contents))
            .unwrap_or(Printed::Same),
        TypeView::Named(n) if n.kind == NamedKind::Enum => Printed::Same,
        TypeView::Named(_) if t.program.analysis.declares_method(ty, "toString") => Printed::Same,
        TypeView::Map(..)
        | TypeView::Set(_)
        | TypeView::Record
        | TypeView::Named(_)
        | TypeView::Promise(_)
        | TypeView::Shared(_)
        | TypeView::Fn => Printed::Contents,
        _ => Printed::Same,
    }
}

/// How an element of an array prints: `null` as empty text in both (JavaScript writes
/// `undefined` that way too), and a class instance by its contents in Velt even when the class
/// declares `toString()`.
fn element_printed(ty: &TypeRef, t: &Typed, depth: u32) -> Printed {
    match t.view(ty) {
        TypeView::Nullable(inner) => element_printed(&inner, t, depth + 1),
        TypeView::Named(n) if n.kind != NamedKind::Enum => Printed::Contents,
        _ => match printed(ty, t, depth + 1) {
            Printed::Nullable => Printed::Same,
            p => p,
        },
    }
}

/// `JSON.stringify(v)` where `v` holds a `Map`.
pub(super) fn call(callee: &ast::Expr, args: &[ast::Expr], t: &mut Typed) {
    let E::Member { object, prop, .. } = &callee.kind else {
        return;
    };
    if prop.name != "stringify" || !is_global(object, "JSON", t) {
        return;
    }
    let Some(arg) = args.first() else { return };
    let Some(ty) = t.type_of(arg) else { return };
    if !holds_map(&ty, t, &mut HashSet::new()) {
        return;
    }
    let message =
        "`JSON.stringify` writes a `Map` as an object in Velt, as `{}` in JavaScript".to_string();
    t.cx.report(
        "json-map",
        Severity::Error,
        arg.span,
        message,
        &[
            "Velt writes a `Map` as an object of its entries; JavaScript's `JSON.stringify` \
             sees no own properties and writes `{}`",
            "write its entries (`[...m.entries()]`), or use an object type instead of a `Map`",
        ],
        None,
    );
}

/// Whether `ty` holds a `Map` (itself, in a field, an element, a union member).
fn holds_map(ty: &TypeRef, t: &Typed, seen: &mut HashSet<String>) -> bool {
    if !seen.insert(t.program.analysis.show_type(ty)) || seen.len() > 64 {
        return false;
    }
    match t.view(ty) {
        TypeView::Map(..) => true,
        TypeView::Nullable(x) | TypeView::Array(x) | TypeView::Shared(x) => holds_map(&x, t, seen),
        TypeView::Tuple(xs) | TypeView::Union(xs) => xs.iter().any(|x| holds_map(x, t, seen)),
        TypeView::Record => fields_hold_map(ty, t, seen),
        TypeView::Named(n) if !n.is_std && n.kind != NamedKind::Enum => {
            fields_hold_map(ty, t, seen)
        }
        _ => false,
    }
}

fn fields_hold_map(ty: &TypeRef, t: &Typed, seen: &mut HashSet<String>) -> bool {
    let fields = t.program.analysis.fields(ty);
    fields.iter().any(|f| holds_map(&f.ty, t, seen))
}
