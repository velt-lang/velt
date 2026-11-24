//! Rules on turning values into text: template literals of promises (`object-in-template`).
//! (`JSON.stringify` writes a `Map` as `{}` in Velt too, as JavaScript does: no rule.)

use velt_sema::ide::{TypeRef, TypeView};
use velt_syntax::ast;

use super::Typed;
use crate::Severity;

/// How a template literal prints a value, compared with JavaScript.
enum Printed {
    /// The same in both: primitives, enums, arrays, and objects, which both write as JS's
    /// `String(x)` does (a class's `toString()`, own or inherited, as `Error`'s `"Error: boom"`,
    /// else `[object Object]`, or `[object Map]` for std's classes); or unknown (a generic
    /// parameter).
    Same,
    /// A `Promise`: Velt prints it as `console.log` does, JavaScript as `[object Promise]`.
    Promise,
}

/// `${x}` in a template literal.
pub(super) fn template(exprs: &[ast::Expr], t: &mut Typed) {
    for e in exprs {
        let Some(ty) = t.type_of(e) else { continue };
        let (what, velt, js, instead) = match printed(&ty, t, 0) {
            Printed::Same => continue,
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
        TypeView::Nullable(inner) => printed(&inner, t, depth + 1),
        TypeView::Union(members) => {
            members
                .iter()
                .map(|m| printed(m, t, depth + 1))
                .fold(Printed::Same, |acc, p| match (acc, p) {
                    (Printed::Promise, _) | (_, Printed::Promise) => Printed::Promise,
                    _ => Printed::Same,
                })
        }
        TypeView::Promise(_) => Printed::Promise,
        // Everything else is written as JS writes it, or (a function) is a compile error in
        // Velt.
        _ => Printed::Same,
    }
}
