//! `velt-global` and `velt-member`: names of the standard prelude and the compiler's builtins
//! that TypeScript doesn't have (`spawn`, `assertEq`, `JsonValue`, `xs.isEmpty()`,
//! `JSON.parse<T>(…)`), as [`prelude`](super::prelude) classifies them. A name the program
//! declares itself is never one: only uses that resolve to the prelude, or to nothing (a
//! builtin), are looked up.

use velt_common::Span;
use velt_sema::ide::{DefKind, LiteralKind, TypeView};
use velt_syntax::ast::{self, ExprKind as E};

use super::numbers::is_global;
use super::prelude::{velt_global, velt_member};
use super::Typed;

/// Builtin types without a definition that TypeScript doesn't have.
const VELT_BUILTIN_TYPES: &[(&str, &str)] = &[
    (
        "shared",
        "keep thread-shared values out of code shared with TypeScript",
    ),
    (
        "Shared",
        "keep thread-shared values out of code shared with TypeScript",
    ),
];

/// A name used as a value: a prelude export or builtin that is Velt's own.
pub(super) fn ident(id: &ast::Ident, t: &mut Typed) {
    if !from_prelude(id.span, t) {
        return;
    }
    let hint = if id.name.starts_with("__") {
        Some("it is the standard library's internal helper; don't call it")
    } else {
        velt_global(&id.name)
    };
    if let Some(hint) = hint {
        global(id, hint, t);
    }
}

/// A written type naming a Velt-only prelude type (`JsonValue`, `Mutex<T>`, `shared<T>`).
pub(super) fn ty(ty: &ast::TypeExpr, t: &mut Typed) {
    let ast::TypeExprKind::Named { path, .. } = &ty.kind else {
        return;
    };
    let [name] = path.as_slice() else { return };
    let def = t.def(name.span);
    let hint = match &def {
        Some(d) if t.is_std(d) && d.kind.is_type() => velt_global(&name.name),
        Some(_) => None,
        None => VELT_BUILTIN_TYPES
            .iter()
            .find(|(n, _)| *n == name.name)
            .map(|(_, hint)| *hint),
    };
    if let Some(hint) = hint {
        global(name, hint, t);
    }
}

/// Whether the name at `span` refers to the prelude or a builtin (not to the program's own
/// declarations).
fn from_prelude(span: Span, t: &Typed) -> bool {
    match t.def(span) {
        Some(d) => t.is_std(&d) && !matches!(d.kind, DefKind::Local | DefKind::Parameter),
        None => true,
    }
}

fn global(name: &ast::Ident, hint: &str, t: &mut Typed) {
    t.cx.error(
        "velt-global",
        name.span,
        format!(
            "`{}` is Velt's own; TypeScript has no such global",
            name.name
        ),
        &[
            "the Velt standard library provides it, but TypeScript's (and the browser's) \
             doesn't, so `tsc` reports an unknown name",
            hint,
        ],
    );
}

/// `x.m` where `m` is a Velt-only member of the prelude type of `x` (or of the prelude class
/// `x` names).
pub(super) fn member(e: &ast::Expr, object: &ast::Expr, prop: &ast::Ident, t: &mut Typed) {
    let optional = matches!(e.kind, E::Member { optional: true, .. });
    let owner = owner(object, optional, t);
    if let Some(hint) = owner.as_deref().and_then(|o| velt_member(o, &prop.name)) {
        let owner = owner.as_deref().unwrap_or_default();
        report_member(prop.span, &format!("{owner}.{}", prop.name), hint, t);
    } else if prop.name == "clone" && t.def(prop.span).is_none() {
        // The compiler's `clone`, which every type has unless it declares its own.
        let owner = match (owner, t.type_of(object)) {
            (Some(owner), _) => owner,
            (None, Some(ty)) => t.program.analysis.show_type(&ty),
            (None, None) => "value".into(),
        };
        report_member(prop.span, &format!("{owner}.clone"), CLONE, t);
    }
}

const CLONE: &str = "copy what you need yourself (`{ ...o }`, `[...xs]`, `new Map(m)`), or \
                     declare a `clone()` method on the class";

fn report_member(span: Span, what: &str, hint: &str, t: &mut Typed) {
    t.cx.error(
        "velt-member",
        span,
        format!("`{what}` is Velt's own; TypeScript doesn't have it"),
        &[
            "the Velt standard library provides it, but TypeScript's doesn't, so `tsc` \
             reports an unknown member",
            hint,
        ],
    );
}

/// The prelude owner of the members of `object`: a prelude class or builtin it names (static
/// members), or the type of its value.
fn owner(object: &ast::Expr, optional: bool, t: &Typed) -> Option<String> {
    if let E::Ident(id) = &object.kind {
        match t.def(id.span) {
            Some(d) if d.kind.is_type() || d.kind == DefKind::Function => {
                return t.is_std(&d).then(|| id.name.clone());
            }
            Some(_) => {}
            None => return Some(id.name.clone()),
        }
    }
    let view = match t.view_of(object) {
        TypeView::Nullable(inner) if optional => t.view(&inner),
        TypeView::Nullable(_) => return Some("nullable".into()),
        view => view,
    };
    let owner = match view {
        TypeView::Array(_) | TypeView::Tuple(_) => "Array",
        TypeView::Int(_)
        | TypeView::Float(_)
        | TypeView::Literal(LiteralKind::Int | LiteralKind::Float) => "number",
        TypeView::Str | TypeView::Literal(LiteralKind::Str) => "string",
        TypeView::Bool | TypeView::Literal(LiteralKind::Bool) => "boolean",
        TypeView::Map(..) => "Map",
        TypeView::Promise(_) => "Promise",
        TypeView::Named(n) if n.is_std => return Some(n.name),
        _ => return None,
    };
    Some(owner.into())
}

/// `JSON.parse<T>(…)`: TypeScript's `JSON.parse` takes no type argument.
pub(super) fn call(callee: &ast::Expr, type_args: &[ast::TypeExpr], t: &mut Typed) {
    let E::Member { object, prop, .. } = &callee.kind else {
        return;
    };
    if type_args.is_empty() || prop.name != "parse" || !is_global(object, "JSON", t) {
        return;
    }
    report_member(
        prop.span,
        "JSON.parse<T>",
        "TypeScript's `JSON.parse` takes no type argument and checks nothing; keep typed \
         decoding out of code shared with TypeScript",
        t,
    );
}
