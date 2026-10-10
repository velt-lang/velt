//! Rules on turning values into text: template literals of errors and promises
//! (`object-in-template`) and of values that may be `null` (`nullable-in-template`), and
//! `JSON.stringify` of a `Map` (`json-map`; a `Set` has no JSON form in Velt).

use std::collections::HashSet;

use velt_sema::ide::{NamedKind, TypeRef, TypeView};
use velt_syntax::ast::{self, ExprKind as E};

use super::numbers::is_global;
use super::{nulls, Typed};
use crate::Severity;

/// How a template literal prints a value, compared with JavaScript.
enum Printed {
    /// The same in both: primitives, enums, arrays, and objects, which both write as JS's
    /// `String(x)` does (a class's `toString()`, own or inherited, else `[object Object]`, or
    /// `[object Map]` for std's classes); or unknown (a generic parameter).
    Same,
    /// The same, except that JavaScript prints `undefined` where the value is `undefined`.
    Nullable,
    /// An `Error`: Velt prints it as `console.log` does, JavaScript as `Error: message`.
    Error,
    /// A `Promise`: Velt prints it as `console.log` does, JavaScript as `[object Promise]`.
    Promise,
}

/// `${x}` in a template literal.
pub(super) fn template(exprs: &[ast::Expr], t: &mut Typed) {
    for e in exprs {
        let Some(ty) = t.type_of(e) else { continue };
        let (what, velt, js, instead) = match printed(&ty, t, 0) {
            Printed::Same => continue,
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
                    "`${…}` of a value that may be `undefined` in JavaScript: it prints                      `undefined` there, `null` in Velt"
                        .into(),
                    &[
                        why,
                        "say what to print for nothing: `${x ?? \"\"}`, or test the value first",
                    ],
                    None,
                );
                continue;
            }
            Printed::Error => (
                "an error",
                "`Error { message: 'boom' }`",
                "`Error: boom`",
                "write its message: `${e.message}`",
            ),
            Printed::Promise => (
                "a `Promise`",
                "`Promise { <pending> }`",
                "`[object Promise]`",
                "`await` it and write its value",
            ),
        };
        t.cx.report(
            "object-in-template",
            Severity::Error,
            e.span,
            format!("`${{…}}` of {what} prints differently in Velt and JavaScript"),
            &[
                &format!("Velt writes it as `console.log` does ({velt}); JavaScript as {js}"),
                instead,
            ],
            None,
        );
    }
}

fn printed(ty: &TypeRef, t: &Typed, depth: u32) -> Printed {
    if depth > 8 {
        return Printed::Same;
    }
    match t.view(ty) {
        TypeView::Nullable(inner) => match printed(&inner, t, depth + 1) {
            Printed::Same | Printed::Nullable => Printed::Nullable,
            p => p,
        },
        TypeView::Union(members) => {
            members
                .iter()
                .map(|m| printed(m, t, depth + 1))
                .fold(Printed::Same, |acc, p| match (acc, p) {
                    (Printed::Error, _) | (_, Printed::Error) => Printed::Error,
                    (Printed::Promise, _) | (_, Printed::Promise) => Printed::Promise,
                    (Printed::Nullable, _) | (_, Printed::Nullable) => Printed::Nullable,
                    _ => Printed::Same,
                })
        }
        // An error class's own `toString()` is called in both.
        TypeView::Named(n)
            if n.kind == NamedKind::Class
                && t.program.analysis.is_error(ty)
                && !t.program.analysis.declares_method(ty, "toString") =>
        {
            Printed::Error
        }
        TypeView::Promise(_) => Printed::Promise,
        // Everything else is written as JS writes it, or (a function) is a compile error in
        // Velt.
        _ => Printed::Same,
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
