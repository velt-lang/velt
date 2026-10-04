//! What goes into the parameters and fields the program declares: `null` for an optional one
//! (`null-into-optional`: TypeScript types it `T | undefined`, without `null`), and a value
//! that is `undefined` in JavaScript for a required `T | null` (`undefined-into-null`, see
//! [`nulls`](super::nulls)). Arguments of functions, methods and constructors, object literal
//! fields and assignments to fields.

use velt_common::Span;
use velt_sema::ide::{DefKind, TypeView};
use velt_syntax::ast::{self, ExprKind as E};

use super::nulls::{check_into_null, ident_source, is_null};
use super::Typed;
use crate::{Fix, Severity};

/// `o.f = v`: `null` into an optional field, or an `undefined` into a required one.
pub(super) fn assign(target: &ast::Expr, value: &ast::Expr, t: &mut Typed) {
    let E::Member { prop, .. } = &target.kind else {
        return;
    };
    let Some(d) = t.def(prop.span).filter(|d| d.kind == DefKind::Field) else {
        return;
    };
    if t.decls.is_optional(d.span) {
        if is_null(value) {
            null_into_optional(value.span, &format!("field `{}`", prop.name), None, t);
        }
    } else {
        check_into_null(value, t);
    }
}

/// `{ f: v }`: the same for the fields of an object literal.
pub(super) fn object(props: &[ast::ObjectProp], t: &mut Typed) {
    for (i, prop) in props.iter().enumerate() {
        let (key, value) = match prop {
            ast::ObjectProp::KeyValue(key, value) => (key, Some(value)),
            ast::ObjectProp::Shorthand(key) => (key, None),
            ast::ObjectProp::Spread(_) | ast::ObjectProp::Method(_) => continue,
        };
        let Some(d) = t.def(key.span).filter(|d| d.kind == DefKind::Field) else {
            continue;
        };
        let optional = t.decls.is_optional(d.span);
        match value {
            Some(v) if optional && is_null(v) => {
                let next = props.get(i + 1).map(|p| prop_span(p).lo);
                let prev = i.checked_sub(1).map(|j| prop_span(&props[j]).hi);
                let removal =
                    list_removal(Span::new(v.span.file, key.span.lo, v.span.hi), prev, next);
                null_into_optional(v.span, &format!("field `{}`", key.name), Some(removal), t);
            }
            Some(v) if !optional => check_into_null(v, t),
            None if !optional => {
                if let Some(why) = ident_source(key, t) {
                    let fix = Fix {
                        span: key.span,
                        replacement: format!("{0}: {0} ?? null", key.name),
                        title: "turn `undefined` into `null`: `?? null`".into(),
                    };
                    t.cx.report(
                        "undefined-into-null",
                        Severity::Error,
                        key.span,
                        "TypeScript types this value `T | undefined`, which a `T | null` \
                         doesn't accept"
                            .into(),
                        &[why, "write `name: name ?? null`"],
                        Some(fix),
                    );
                }
            }
            _ => {}
        }
    }
}

fn prop_span(p: &ast::ObjectProp) -> Span {
    match p {
        ast::ObjectProp::KeyValue(k, v) => Span::new(k.span.file, k.span.lo, v.span.hi),
        ast::ObjectProp::Shorthand(k) => k.span,
        ast::ObjectProp::Spread(e) => e.span,
        ast::ObjectProp::Method(f) => Span::new(f.sig.span.file, f.sig.span.lo, f.body.span.hi),
    }
}

/// Removing the list item at `item` (an argument or a property): with the separator after it,
/// or, for the last item, the one before it.
fn list_removal(item: Span, prev_end: Option<u32>, next_start: Option<u32>) -> Fix {
    let span = match (next_start, prev_end) {
        (Some(next), _) => Span::new(item.file, item.lo, next),
        (None, Some(prev)) => Span::new(item.file, prev, item.hi),
        (None, None) => item,
    };
    Fix {
        span,
        replacement: String::new(),
        title: "leave it out".into(),
    }
}

fn null_into_optional(span: Span, what: &str, fix: Option<Fix>, t: &mut Typed) {
    let notes = [
        "TypeScript types an optional parameter or field `T | undefined`, which doesn't accept \
         `null`; Velt's `x?: T` is `T | null`",
        "leave the value out: both languages then see it as absent",
    ];
    t.cx.report(
        "null-into-optional",
        Severity::Error,
        span,
        format!("`null` for the optional {what}, which TypeScript doesn't accept"),
        &notes,
        fix,
    );
}

/// The arguments of a call to a function or method declared in the program.
pub(super) fn call(callee: &ast::Expr, args: &[ast::Expr], t: &mut Typed) {
    let name = match &callee.kind {
        E::Ident(id) => id.span,
        E::Member { prop, .. } => prop.span,
        _ => return,
    };
    let Some(d) = t.def(name) else { return };
    if matches!(
        d.kind,
        DefKind::Function | DefKind::Method | DefKind::StaticMethod
    ) {
        arguments(d.span, args, t);
    }
}

/// `new C(…)`: the constructor's arguments.
pub(super) fn new(class: &ast::TypeExpr, args: &[ast::Expr], t: &mut Typed) {
    let ast::TypeExprKind::Named { path, .. } = &class.kind else {
        return;
    };
    let Some(name) = path.last() else { return };
    if let Some(d) = t.def(name.span).filter(|d| d.kind == DefKind::Class) {
        arguments(d.span, args, t);
    }
}

fn arguments(owner: Span, args: &[ast::Expr], t: &mut Typed) {
    if args.iter().any(|a| matches!(a.kind, E::Spread(_))) {
        return;
    }
    let Some(params) = t.decls.params.get(&owner).cloned() else {
        return;
    };
    super::numbers::arguments(&params, args, t);
    let null_for_optional =
        |i: usize| params.get(i).is_some_and(|p| t.decls.is_optional(*p)) && is_null(&args[i]);
    // Trailing `null`s for optional parameters can be left out: the first one's fix removes
    // them all.
    let trailing = (0..args.len())
        .rev()
        .take_while(|&i| null_for_optional(i))
        .last();
    for (i, (arg, param)) in args.iter().zip(&params).enumerate() {
        if null_for_optional(i) {
            let fix = (trailing == Some(i)).then(|| {
                let prev = i.checked_sub(1).map(|j| args[j].span.hi);
                let last = args[args.len() - 1].span.hi;
                let mut f = list_removal(arg.span, prev, None);
                f.span = Span::new(f.span.file, f.span.lo, last);
                f
            });
            null_into_optional(arg.span, "parameter", fix, t);
        } else if t.decls.is_optional(*param) {
            continue;
        } else if param_nullable(*param, t) {
            check_into_null(arg, t);
        }
    }
}

/// The parameter declared at `param` has a `T | null` type.
fn param_nullable(param: Span, t: &Typed) -> bool {
    t.program
        .analysis
        .type_of(param)
        .is_some_and(|ty| matches!(t.view(&ty), TypeView::Nullable(_)))
}
